use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::ops::Range;

#[derive(Debug, PartialEq)]
struct Metadata {
    name: String,
    maternal: String,
    paternal: String,
    dataset_type: String, // "intercross" or "riset"
    heterozygous: String, // defaults to "H"
    unknown: String,      // defaults to "U"
}

impl Metadata {
    fn parse_genotype(&self, geno: &str) -> (Genotype, f64) {
        if geno == self.maternal.as_str() {
            (Genotype::Mat, -1.0)
        } else if geno == self.paternal.as_str() {
            (Genotype::Pat, 1.0)
        } else if geno == self.heterozygous.as_str() {
            (Genotype::Het, 0.0)
        } else if geno == self.unknown.as_str() {
            (Genotype::Unk, 99.0)
        } else {
            panic!("Failed to parse genotype: {}\n{:?}", geno, self);
        }
    }

    fn parse_dominance(&self, geno: &str) -> f64 {
        if geno == self.maternal.as_str() {
            0.0
        } else if geno == self.paternal.as_str() {
            0.0
        } else if geno == self.heterozygous.as_str() {
            1.0
        } else if geno == self.unknown.as_str() {
            1.0
        } else {
            panic!("Failed to parse genotype: {}\n{:?}", geno, self);
        }
    }

    fn parse_line(line: &str) -> Option<(&str, &str)> {
        if line.starts_with("#") {
            return None;
        }

        if line.starts_with("@") {
            let sep = line.find(':').unwrap();
            let name = &line[1..sep];
            let val = &line[sep + 1..];

            return Some((name, val));
        }

        None
    }

    // panic!s if the provided lines do not contain @name, @mat, and @pat fields
    fn from_lines(lines: Vec<&str>) -> Metadata {
        let mut name: Option<String> = None;
        let mut mat: Option<String> = None;
        let mut pat: Option<String> = None;

        // the type should be either `riset` or `intercross`; fix later
        let mut typ: Option<String> = None;
        let mut het = String::from("H");
        let mut unk = String::from("U");

        for line in lines.iter() {
            if let Some((n, v)) = Metadata::parse_line(line) {
                match n {
                    "name" => name = Some(String::from(v)),
                    "mat" => mat = Some(String::from(v)),
                    "pat" => pat = Some(String::from(v)),
                    "type" => typ = Some(String::from(v)),
                    "het" => het = String::from(v),
                    "unk" => unk = String::from(v),
                    _ => (),
                }
            }
        }

        if name == None || mat == None || pat == None || typ == None {
            panic!(
                "Required metadata was not provided!\nname = {:?}\nmat = {:?}\npat = {:?}\ntype = {:?}",
                name, mat, pat, typ
            );
        }

        Metadata {
            name: name.unwrap(),
            maternal: mat.unwrap(),
            paternal: pat.unwrap(),
            dataset_type: typ.unwrap(),
            heterozygous: het,
            unknown: unk,
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct Marker {
    pub name: String,
    pub centi_morgan: f64,
    pub mega_basepair: Option<f64>,
    pub chromosome: String,
}

#[derive(Debug, PartialEq)]
pub struct Locus {
    dominance: Option<Vec<f64>>,
    genotype: Vec<(Genotype, f64)>,
    pub marker: Marker,
}

/// UnknownIntervals holds a list of ranges of unknown genotypes, per strain
struct UnknownIntervals(Vec<Vec<Range<usize>>>);

impl Locus {
    // corresponds to lines 950-1044 in dataset.c
    fn parse_line(
        metadata: &Metadata,
        has_mb: bool,
        // header: &DatasetHeader,
        dominance: bool,
        line: &str,
    ) -> (String, Locus) {
        // Example locus is: "1	D1Mit1	8.3	B6	B6	D	D"
        // where the first three columns are chromosome, name, cM;
        // remaining columns are the genotypes

        let words: Vec<_> = line.split_terminator('\t').collect();

        let chromosome = String::from(words[0]);
        let name = String::from(words[1]);
        let centi_morgan = words[2].parse::<f64>().unwrap();
        let mega_basepair = if has_mb {
            words[3].parse::<f64>().ok()
        } else {
            None
        };

        let marker = Marker {
            name,
            centi_morgan,
            mega_basepair,
            chromosome: chromosome.clone(),
        };

        let range = if has_mb { 4.. } else { 3.. };

        let genotype = words[range.clone()]
            .iter()
            .map(|g| metadata.parse_genotype(g))
            .collect();

        let dominance = if dominance {
            Some(
                words[range]
                    .iter()
                    .map(|g| metadata.parse_dominance(g))
                    .collect(),
            )
        } else {
            None
        };

        (
            chromosome,
            Locus {
                genotype,
                dominance,
                marker,
            },
        )
    }

    /// Steps through a list of genotypes per strain, building up a list of ranges of missing data for each strain
    fn step_many_unknown_intervals_mut(
        state: &mut (Vec<Option<usize>>, Vec<Vec<Range<usize>>>),
        next: (usize, &[(Genotype, f64)]),
    ) {
        let (ix, genotype) = next;

        for (strain_ix, (geno, _)) in genotype.iter().enumerate() {
            if let Genotype::Unk = geno {
                match state.0[strain_ix] {
                    None => state.0[strain_ix] = Some(ix),
                    Some(start) => state.0[strain_ix] = Some(start),
                }
            } else {
                if let Some(start) = state.0[strain_ix] {
                    state.1[strain_ix].push(start..ix);
                    state.0[strain_ix] = None;
                }
            }
        }
    }

    fn find_unknown_intervals(loci: &[Locus]) -> UnknownIntervals {
        let n_strains = loci.first().unwrap().genotype.len();
        let mut state = (vec![None; n_strains], vec![Vec::new(); n_strains]);

        for (locus_ix, locus) in loci.iter().enumerate() {
            Self::step_many_unknown_intervals_mut(&mut state, (locus_ix, &locus.genotype))
        }

        UnknownIntervals(state.1)
    }

    fn estimate_unknown_genotypes(
        dominance: bool,
        loci: &mut [Locus],
        intervals: UnknownIntervals,
    ) {
        for (strain_ix, strain) in intervals.0.iter().enumerate() {
            for range in strain {
                for locus_ix in range.clone() {
                    let prev = &loci[range.start - 1];
                    let next = &loci[range.end];
                    let locus = &loci[locus_ix];
                    let rec_1 = (locus.cm() - prev.cm()) / 100.0;
                    let rec_2 = (next.cm() - locus.cm()) / 100.0;
                    let rec_0 = (next.cm() - prev.cm()) / 100.0;

                    let f1 = (1.0 - f64::exp(-2.0 * rec_1)) / 2.0;
                    let f2 = (1.0 - f64::exp(-2.0 * rec_2)) / 2.0;
                    let f0 = (1.0 - f64::exp(-2.0 * rec_0)) / 2.0;

                    // NOTE make sure the parens act the same as the C version!!
                    let r_0 = (1.0 - f1) * (1.0 - f2) / (1.0 - f0);
                    let r_1 = f1 * (1.0 - f2) / f0;
                    let r_2 = f2 * (1.0 - f1) / f0;
                    let r_3 = f1 * f2 / (1.0 - f0);

                    let (prev_geno, _) = prev.genotype[strain_ix];
                    let (next_geno, _) = next.genotype[strain_ix];

                    // use Genotype::*;

                    let new_genotype = match (prev_geno, next_geno) {
                        (Genotype::Mat, Genotype::Mat) => 1.0 - 2.0 * r_0,
                        (Genotype::Het, Genotype::Mat) => 1.0 - r_0 - r_1,
                        (Genotype::Pat, Genotype::Mat) => 1.0 - 2.0 * r_1,
                        (Genotype::Mat, Genotype::Het) => r_1 - r_0,
                        (Genotype::Het, Genotype::Het) => 0.0,
                        (Genotype::Pat, Genotype::Het) => r_0 - r_1,
                        (Genotype::Mat, Genotype::Pat) => 2.0 * r_1 - 1.0,
                        (Genotype::Het, Genotype::Pat) => r_0 + r_1 - 1.0,
                        (Genotype::Pat, Genotype::Pat) => 2.0 * r_0 - 1.0,
                        _ => panic!("Genotype was unknown when it shouldn't be!"),
                    };

                    if dominance {
                        let new_dominance = match (prev_geno, next_geno) {
                            (Genotype::Mat, Genotype::Mat) => 2.0 * r_0 * r_3,
                            (Genotype::Het, Genotype::Mat) => r_1 * (r_2 + r_3),
                            (Genotype::Pat, Genotype::Mat) => 2.0 * r_1 * r_2,
                            (Genotype::Mat, Genotype::Het) => r_1 * r_0 + r_2 * r_3,
                            (Genotype::Het, Genotype::Het) => {
                                let w = ((1.0 - f0) * (1.0 - f0)) / (1.0 - 2.0 * f0 * (1.0 - f0));
                                1.0 - 2.0 * w * r_0 * r_3 - 2.0 * (1.0 - w) * r_1 * r_2
                            }
                            (Genotype::Pat, Genotype::Het) => r_0 * r_1 + r_2 * r_3,
                            (Genotype::Mat, Genotype::Pat) => 2.0 * r_1 * r_2,
                            (Genotype::Het, Genotype::Pat) => r_1 * (r_2 + r_3),
                            (Genotype::Pat, Genotype::Pat) => 2.0 * r_1 * r_3,
                            _ => panic!("Genotype was unknown when it shouldn't be!"),
                        };

                        if let Some(d) = &mut loci[locus_ix].dominance {
                            d[strain_ix] = new_dominance;
                        }
                    }

                    loci[locus_ix].genotype[strain_ix].1 = new_genotype
                }
            }
        }
    }

    pub fn cm(&self) -> f64 {
        self.marker.centi_morgan
    }

    pub fn genotypes_subset(&self, strain_ixs: &[usize]) -> Vec<(Genotype, f64)> {
        strain_ixs.iter().map(|ix| self.genotype[*ix]).collect()
    }
}

pub struct Genome {
    chr_order: Vec<String>,
    chromosomes: HashMap<String, Vec<Locus>>, // chromosomes: Vec<(String, Vec<Locus>)>
}

pub struct GenomeIter<'a> {
    keys: Vec<String>,
    chromosomes: &'a HashMap<String, Vec<Locus>>,
}

impl<'a> Iterator for GenomeIter<'a> {
    type Item = &'a Vec<Locus>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.keys.len() == 0 {
            None
        } else {
            let chr = self.keys.remove(0);
            self.chromosomes.get(&chr)
        }
    }
}

impl Genome {
    fn new() -> Genome {
        Genome {
            chr_order: Vec::new(),
            chromosomes: HashMap::new(),
        }
    }

    fn or_push_chromosome(&mut self, chr: String) -> &mut Vec<Locus> {
        if let None = self.chr_order.iter().find(|&c| c == &chr) {
            self.chr_order.push(chr.clone());
        }

        self.chromosomes.entry(chr).or_insert_with(|| Vec::new())
    }

    fn push_locus(&mut self, chr: String, locus: Locus) {
        self.or_push_chromosome(chr).push(locus);
    }

    /// Iterates through the chromosomes in the order they were added to the genotype
    pub fn iter<'a>(&'a self) -> GenomeIter<'a> {
        GenomeIter {
            keys: self.chr_order.clone(),
            chromosomes: &self.chromosomes,
        }
    }

    /// Mutably iterates through the chromosomes, using the arbitrary order from HashMap
    fn iter_mut<'a>(&'a mut self) -> impl Iterator<Item = (&'a String, &'a mut Vec<Locus>)> {
        self.chromosomes.iter_mut()
    }
}

#[derive(Debug, PartialEq, PartialOrd, Clone, Copy)]
pub enum Genotype {
    Mat,
    Pat,
    Het,
    Unk,
}

// #[derive(Debug)]
pub struct Dataset {
    metadata: Metadata,
    has_mb: bool,
    pub genome: Genome,
    strains: Vec<String>,
    dominance: bool, // true if dataset type is "intercross"
}

impl Dataset {
    fn new(metadata: Metadata, has_mb: bool, strains: Vec<String>) -> Dataset {
        let dominance = metadata.dataset_type == String::from("intercross");
        Dataset {
            metadata,
            has_mb,
            strains,
            genome: Genome::new(),
            dominance,
        }
    }

    pub fn strains(&self) -> &[String] {
        &self.strains
    }

    pub fn strain_indices(&self, strains: &[String]) -> Vec<usize> {
        strains
            .iter()
            .map(|s| self.strains.iter().position(|p| p == s).unwrap())
            .collect()
    }

    pub fn n_loci(&self) -> usize {
        self.genome
            .chromosomes
            .iter()
            .map(|(_, loci)| loci.len())
            .sum()
    }

    fn parse_dataset_header(line: &str) -> (bool, Vec<String>) {
        let header_words: Vec<_> = line.split_terminator('\t').collect();

        let has_mb = match header_words.get(3) {
            None => panic!("Dataset header had less than four elements; no strains!"),
            Some(w) => *w == "Mb",
        };

        let skip_n = if has_mb { 4 } else { 3 };

        let strains = header_words
            .into_iter()
            .skip(skip_n)
            .map(|s| String::from(s))
            .collect();

        (has_mb, strains)
    }

    pub fn read_file(path: &str) -> Dataset {
        let f = File::open(path).expect(&format!("Error opening file {}", path));

        let reader = BufReader::new(f);
        let mut lines = reader.lines();

        let has_mb;
        let strains;

        let mut metadata_lines = vec![];

        loop {
            match lines.next() {
                None => panic!("Reached end of file before parsing dataset header"),
                Some(l) => {
                    let ll = l.unwrap();
                    if ll.starts_with("Chr	Locus	cM") {
                        let header = Dataset::parse_dataset_header(&ll);
                        has_mb = header.0;
                        strains = header.1;
                        break;
                    } else {
                        metadata_lines.push(ll);
                    }
                }
            }
        }

        let metadata = Metadata::from_lines(metadata_lines.iter().map(|s| s.as_str()).collect());

        let mut dataset = Dataset::new(metadata, has_mb, strains);

        for line in lines {
            let (chr, locus) =
                Locus::parse_line(&dataset.metadata, has_mb, dataset.dominance, &line.unwrap());
            dataset.genome.push_locus(chr, locus);
        }
        dataset.estimate_unknown();

        dataset
    }

    // Corresponds to lines 1071-1152 in dataset.c
    fn estimate_unknown(&mut self) {
        // first replace any cases of "Unknown" in the first and last loci of each chromosome

        let replace = |geno: &mut Genotype, val: &mut f64| {
            if let Genotype::Unk = *geno {
                *geno = Genotype::Het;
                *val = 0.0;
            }
        };

        for (_chr, loci) in self.genome.iter_mut() {
            let replace_genotype = |locus: Option<&mut Locus>| {
                locus
                    .unwrap()
                    .genotype
                    .iter_mut()
                    .for_each(|(geno, val)| replace(geno, val))
            };

            replace_genotype(loci.first_mut());
            replace_genotype(loci.last_mut());
        }

        for (_chr, loci) in self.genome.iter_mut() {
            // then, for each chromosome, construct the intervals of
            // unknown genotypes
            let unk = Locus::find_unknown_intervals(loci);

            // ... and use those intervals to estimate the
            // missing genotypes
            Locus::estimate_unknown_genotypes(self.dominance, loci, unk);
        }
    }
}

#[derive(Debug)]
pub struct QTL {
    pub lrs: f64,
    pub additive: f64,
    pub dominance: Option<f64>,
    pub marker: Marker,
}

impl QTL {
    pub fn new(marker: Marker, lrs: f64, additive: f64, dominance: Option<f64>) -> QTL {
        QTL {
            lrs,
            additive,
            dominance,
            marker,
        }
    }
}

pub struct Traits {
    pub strains: Vec<String>,
    pub traits: Vec<(String, Vec<f64>)>,
}

impl Traits {
    pub fn read_file(path: &str) -> Traits {
        let f = File::open(path).expect(&format!("Error opening traits file {}", path));

        let reader = BufReader::new(f);
        let mut lines = reader.lines();

        let strains = match lines.next() {
            None => panic!("Reached end of file before parsing traits header"),
            Some(l) => {
                let ll = l.unwrap();
                if ll.starts_with("Trait") {
                    ll.split_terminator('\t')
                        .skip(1)
                        .map(|s| s.to_string())
                        .collect()
                } else {
                    panic!("Traits file did not begin with \"Trait\", aborting");
                }
            }
        };

        // let mut traits = HashMap::new();
        let mut traits = Vec::new();

        for line in lines {
            let ll = line.unwrap();
            let mut words = ll.split_terminator('\t');
            let key = words.next().unwrap().to_string();
            let values = words.map(|s| s.parse::<f64>().unwrap()).collect();
            traits.push((key, values));
        }

        println!("parsed strains: {:?}", strains);
        println!("parsed traits: {:?}", traits);

        Traits { strains, traits }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_line() -> String {
        String::from("Chr	Locus	cM	BXD1	BXD2	BXD5	BXD6")
    }

    #[test]
    fn it_can_parse_header() {
        let header = header_line();

        let (has_mb_1, strains_1) = Dataset::parse_dataset_header(&header);

        assert_eq!(false, has_mb_1);
        assert_eq!(vec!["BXD1", "BXD2", "BXD5", "BXD6"], strains_1);

        let (has_mb_2, strains_2) = Dataset::parse_dataset_header(&header);

        assert_eq!(true, has_mb_2);
        assert_eq!(vec!["BXD1", "BXD2", "BXD5", "BXD6"], strains_2);
    }

    #[test]
    fn it_can_parse_metadata_lines() {
        let lines = vec![
            "#@type:intercross",
            "@name:BXD",
            "#abbreviation of maternal or paternal parents",
            "@mat:B6",
        ];

        assert_eq!(Metadata::parse_line(lines[0]), None);
        assert_eq!(Metadata::parse_line(lines[1]), Some(("name", "BXD")));
        assert_eq!(Metadata::parse_line(lines[2]), None);
        assert_eq!(Metadata::parse_line(lines[3]), Some(("mat", "B6")));
    }

    #[test]
    fn it_can_parse_metadata() {
        let lines = vec![
        "#comment line always start with a '#'",
        "#type riset or intercross",
        "@type:riset",
        "@name:BXD",
        "#abbreviation of maternal or paternal parents",
        "@mat:B6",
        "@pat:D",
        "#heterozygous , optional, default is \"H\"",
        "@het:H",
        "#Unknown , optional, default is \"U\"",
        "@unk:U",
        "Chr	Locus	cM	BXD1	BXD2	BXD5	BXD6	BXD8	BXD9	BXD11	BXD12	BXD13	BXD14	BXD15	BXD16	BXD18	BXD19	BXD20	BXD21	BXD22	BXD23	BXD24	BXD25	BXD27	BXD28	BXD29	BXD30	BXD31	BXD32	BXD33	BXD34	BXD35	BXD36	BXD37	BXD38	BXD39	BXD40	BXD42",
            ];

        assert_eq!(
            Metadata::from_lines(lines),
            Metadata {
                name: String::from("BXD"),
                maternal: String::from("B6"),
                paternal: String::from("D"),
                dataset_type: String::from("riset"),
                heterozygous: String::from("H"),
                unknown: String::from("U"),
            }
        );
    }

    #[test]
    fn it_can_find_unknown_intervals_in_many_strains() {
        let genos = vec![
            vec![
                (Genotype::Mat, -1.0),
                (Genotype::Mat, -1.0),
                (Genotype::Pat, 1.0),
            ],
            vec![
                (Genotype::Unk, 99.0),
                (Genotype::Pat, 1.0),
                (Genotype::Unk, 99.0),
            ],
            vec![
                (Genotype::Unk, 99.0),
                (Genotype::Unk, 99.0),
                (Genotype::Pat, 1.0),
            ],
            vec![
                (Genotype::Unk, 99.0),
                (Genotype::Unk, 99.0),
                (Genotype::Unk, 99.0),
            ],
            vec![
                (Genotype::Pat, 1.0),
                (Genotype::Mat, -1.0),
                (Genotype::Unk, 99.0),
            ],
            vec![
                (Genotype::Pat, 1.0),
                (Genotype::Mat, -1.0),
                (Genotype::Mat, -1.0),
            ],
        ];

        let strains = 3;
        let mut state = (vec![None; strains], vec![Vec::new(); strains]);

        for (geno_ix, genos_line) in genos.iter().enumerate() {
            Locus::step_many_unknown_intervals_mut(&mut state, (geno_ix, &genos_line));
        }

        assert_eq!(state.1, vec![vec![1..4], vec![2..4], vec![1..2, 3..5]]);
    }

    #[test]
    fn it_can_estimate_unknown_genotypes() {
        // let mut chromosomes = HashMap::new();
        let strains = vec!["S1".to_string(), "S2".to_string(), "S3".to_string()];

        let genos = vec![
            vec![
                (Genotype::Mat, -1.0),
                (Genotype::Mat, -1.0),
                (Genotype::Pat, 1.0),
            ],
            vec![
                (Genotype::Unk, 99.0),
                (Genotype::Pat, 1.0),
                (Genotype::Unk, 99.0),
            ],
            vec![
                (Genotype::Unk, 99.0),
                (Genotype::Unk, 99.0),
                (Genotype::Pat, 1.0),
            ],
            vec![
                (Genotype::Unk, 99.0),
                (Genotype::Unk, 99.0),
                (Genotype::Unk, 99.0),
            ],
            vec![
                (Genotype::Pat, 1.0),
                (Genotype::Mat, -1.0),
                (Genotype::Unk, 99.0),
            ],
            vec![
                (Genotype::Pat, 1.0),
                (Genotype::Mat, -1.0),
                (Genotype::Mat, -1.0),
            ],
        ];

        let mk_locus = |name, cm, genotype| Locus {
            marker: Marker {
                name: String::from(name),
                centi_morgan: cm,
                mega_basepair: None,
                chromosome: String::from("1"),
            },
            dominance: None,
            genotype,
        };

        let loci_new = vec![
            mk_locus(
                "Mk1",
                10.0,
                vec![
                    (Genotype::Mat, -1.0),
                    (Genotype::Mat, -1.0),
                    (Genotype::Pat, 1.0),
                ],
            ),
            mk_locus(
                "Mk2",
                30.3,
                vec![
                    (Genotype::Unk, -0.18523506128077272),
                    (Genotype::Pat, 1.0),
                    (Genotype::Unk, 0.9616255554798838),
                ],
            ),
            mk_locus(
                "Mk3",
                40.1,
                vec![
                    (Genotype::Unk, 0.18906668494617707),
                    (Genotype::Unk, 0.3421367343627405),
                    (Genotype::Pat, 1.0),
                ],
            ),
            mk_locus(
                "Mk4",
                50.2,
                vec![
                    (Genotype::Unk, 0.5826065314914579),
                    (Genotype::Unk, -0.3223330030526561),
                    (Genotype::Unk, 0.3223330030526552),
                ],
            ),
            mk_locus(
                "Mk5",
                60.3,
                vec![
                    (Genotype::Pat, 1.0),
                    (Genotype::Mat, -1.0),
                    (Genotype::Unk, -0.34213673436274084),
                ],
            ),
            mk_locus(
                "Mk6",
                70.1,
                vec![
                    (Genotype::Pat, 1.0),
                    (Genotype::Mat, -1.0),
                    (Genotype::Mat, -1.0),
                ],
            ),
        ];

        let mut loci = vec![
            mk_locus("Mk1", 10.0, genos[0].clone()),
            mk_locus("Mk2", 30.3, genos[1].clone()),
            mk_locus("Mk3", 40.1, genos[2].clone()),
            mk_locus("Mk4", 50.2, genos[3].clone()),
            mk_locus("Mk5", 60.3, genos[4].clone()),
            mk_locus("Mk6", 70.1, genos[5].clone()),
        ];

        let unk = Locus::find_unknown_intervals(&loci);

        Locus::estimate_unknown_genotypes(false, &mut loci, unk);

        assert_eq!(loci, loci_new);
    }

}
