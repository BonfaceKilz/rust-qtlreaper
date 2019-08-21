use crate::geneobject::{Dataset, QTL};
use rand::Rng;
use rayon::prelude::*;

const PERMUTATION_TESTSIZE: usize = 1000;
const BOOTSTRAP_TESTSIZE: usize = 1000;
const MAXPERMUTATION: usize = 1000000;

pub struct RegResult {
    lrs: f64,
    additive: f64,
    dominance: Option<f64>,
}

fn permuted_mut<T>(data: &mut [T]) {
    let n = data.len();
    for ix in 0..n {
        let j = rand::thread_rng().gen_range(0, n);
        data.swap(ix, j);
    }
}

fn bootstrap_indices<T>(v: &[T]) -> Vec<usize> {
    let n = v.len();
    (0..n).map(|_| rand::thread_rng().gen_range(0, n)).collect()
}

pub fn pvalue(lrs: f64, permutations: &[f64]) -> f64 {
    let mut temp = Vec::from(permutations);
    temp.sort_by(|x, y| x.partial_cmp(y).unwrap());

    let i = temp.iter().take_while(|v| **v <= lrs).count();
    let n = permutations.len();

    // clamp output in [0.0, 1.0] in case of NaN (should never happen)
    (1.0 - ((i as f64) / (n as f64))).max(0.0).min(1.0)
}

// TODO: add support for variance and control
// TODO: add support for providing a list of strain names to include
pub fn regression(
    dataset: &Dataset,
    traits: &[f64],
    strains: &[String],
    control: Option<&str>,
) -> Vec<QTL> {
    //
    let mut result = Vec::with_capacity(dataset.n_loci());

    let strain_ixs = dataset.strain_indices(strains);

    let control_geno: Option<Vec<_>> = control.map(|c| {
        dataset
            .genome
            .find_locus(c)
            .unwrap()
            .genotypes_subset(&strain_ixs)
    });

    if control != None && control_geno == None {
        panic!("Control could not be found in loci list");
    }

    for (_, loci) in dataset.genome.chromosomes.iter() {
        for locus in loci.iter() {
            let genotypes = locus.genotypes_subset(&strain_ixs);

            let reg_result = match &control_geno {
                None => {
                    if dataset.dominance {
                        let dominance = locus.dominance_subset(&strain_ixs);
                        regression_3n(traits, &genotypes, &dominance, false)
                    } else {
                        regression_2n(traits, &genotypes)
                    }
                }
                Some(c) => {
                    if dataset.dominance {
                        panic!(
                            "reaper: no composite regression for intercross"
                        );
                    } else {
                        regression_3n(traits, &genotypes, &c, true)
                    }
                }
            };

            result.push(QTL {
                lrs: reg_result.lrs,
                additive: reg_result.additive,
                dominance: reg_result.dominance,
                marker: locus.marker.clone(),
            })
        }
    }

    result
}

pub fn permutation(
    dataset: &Dataset,
    traits: &[f64],
    strains: &[String],
    n_perms: usize,
    threads: usize,
) -> Vec<f64> {
    let threads = threads.max(1);
    // let lrs_thresh = -1.0;
    // let top_n = 10;

    let strain_ixs = dataset.strain_indices(strains);

    let mut vecs = Vec::with_capacity(threads);
    vecs.par_extend((0..threads).into_par_iter().map(|_| {
        let mut temp_vec = Vec::with_capacity(n_perms / 4);
        let mut p_traits = Vec::from(traits);
        permuted_mut(&mut p_traits);
        (0..(n_perms / threads)).for_each(|_| {
            let mut lrs_max = 0.0;
            let mut genotypes = vec![0.0; strain_ixs.len()];

            for (_, loci) in dataset.genome.chromosomes.iter() {
                for locus in loci.iter() {
                    locus.genotypes_subindices(&strain_ixs, &mut genotypes);
                    let reg_result = regression_2n(&p_traits, &genotypes);
                    lrs_max = reg_result.lrs.max(lrs_max);
                }
            }
            temp_vec.push(lrs_max);

            permuted_mut(&mut p_traits);
        });
        temp_vec.into_iter()
    }));
    let mut lrs_vec: Vec<_> = vecs.into_iter().flatten().collect();

    lrs_vec.sort_by(|x, y| x.partial_cmp(y).unwrap());
    lrs_vec
}

pub fn bootstrap(
    dataset: &Dataset,
    traits: &[f64],
    strains: &[String],
    control: Option<&str>,
    n_boot: usize,
) -> Vec<usize> {
    let strain_ixs = dataset.strain_indices(strains);
    let n = traits.len();
    let n_loci = dataset.n_loci();

    let n_test = n_boot.max(BOOTSTRAP_TESTSIZE).min(MAXPERMUTATION);

    let mut locus_count = vec![0; n_loci];

    let control_geno: Option<Vec<_>> = control.map(|c| {
        dataset
            .genome
            .find_locus(c)
            .unwrap()
            .genotypes_subset(&strain_ixs)
    });

    for i in 0..n_test {
        let indices = bootstrap_indices(traits);
        let b_traits: Vec<_> =
            indices.iter().cloned().map(|ix| traits[ix]).collect();

        let mut lrs_max = 0.0;
        let mut l = 0;
        let mut lrs_max_pos = 0;

        for (_, loci) in dataset.genome.chromosomes.iter() {
            for locus in loci.iter() {
                let genotypes = locus.genotypes_subset(&strain_ixs);
                let b_genotypes: Vec<_> =
                    indices.iter().cloned().map(|ix| genotypes[ix]).collect();

                let reg_result = if let Some(control) = &control_geno {
                    let b_control: Vec<_> =
                        indices.iter().cloned().map(|ix| control[ix]).collect();
                    regression_3n(&b_traits, &b_genotypes, &b_control, true)
                } else {
                    // TODO variance
                    regression_2n(&b_traits, &b_genotypes)
                };

                if lrs_max < reg_result.lrs {
                    lrs_max_pos = l;
                    lrs_max = reg_result.lrs;
                }

                l += 1;
            }
        }
        locus_count[lrs_max_pos] += 1;
    }

    locus_count
}

// `traits` corresponds to `YY`
// `genotypes` corresponds to `XX`
fn regression_2n(traits: &[f64], genotypes: &[f64]) -> RegResult {
    let mut sig_y = 0.0;
    let mut sig_yy = 0.0;

    let mut sig_x = 0.0;
    let mut sig_xx = 0.0;
    let mut sig_xy = 0.0;

    let n_strains = traits.len();
    let n = n_strains as f64;

    for ix in 0..traits.len() {
        let temp_trait = traits[ix];
        let temp_geno = genotypes[ix];

        sig_y += temp_trait;
        sig_yy += temp_trait * temp_trait;
        sig_xy += temp_trait * temp_geno;

        sig_x += temp_geno;
        sig_xx += temp_geno * temp_geno;
    }

    let d = sig_xx - sig_x * sig_x / n;
    let tss = sig_yy - (sig_y * sig_y) / n;

    let a = (sig_xx * sig_y - sig_x * sig_xy) / (n * d);
    let mut b = (sig_xy - (sig_x * sig_y / n)) / d;

    let rss = sig_yy
        + a * (n * a - 2.0 * sig_y)
        + b * (2.0 * a * sig_x + b * sig_xx - 2.0 * sig_xy);

    let mut lrs = n * (tss / rss).ln();

    if lrs.is_nan() || lrs < 0.0 {
        b = 0.0;
        lrs = 0.0;
    }

    RegResult {
        lrs,
        additive: b,
        dominance: None,
    }
}

fn regression_2n_variance(
    traits: &[f64],
    genotypes: &[f64],
    variance: &[f64],
) -> RegResult {
    let mut sig_yv = 0.0;
    let mut sig_yyv = 0.0;
    let mut sig_xv = 0.0;
    let mut sig_xxv = 0.0;
    let mut sig_xyv = 0.0;

    let mut sig_1v = 0.0;

    let n_strains = traits.len();

    for ix in 0..traits.len() {
        let temp0 = 1.0 / variance[ix];
        let temp1 = traits[ix];
        let temp2 = genotypes[ix];
        sig_1v += temp0;
        let temp = temp1 * temp0;
        sig_yv += temp;
        sig_yyv += temp1 * temp;
        sig_xyv += temp * temp2;
        let temp = temp2 * temp0;
        sig_xv += temp;
        sig_xxv += temp * temp2;
    }

    let d = sig_xxv - sig_xv * sig_xv / sig_1v;
    let tss = sig_yyv - (sig_yv * sig_yv) / sig_1v;
    let a = (sig_xxv * sig_yv - sig_xv * sig_xyv) / (sig_1v * d);
    let mut b = (sig_xyv - (sig_xv * sig_yv / sig_1v)) / d;
    let rss = sig_yyv
        + a * (sig_1v * a - 2.0 * sig_yv)
        + b * (2.0 * a * sig_xv + b * sig_xxv - 2.0 * sig_xyv);
    let mut lrs = (n_strains as f64) * (tss / rss).ln();

    if lrs.is_nan() || lrs < 0.0 {
        b = 0.0;
        lrs = 0.0;
    }

    RegResult {
        lrs,
        additive: b,
        dominance: None,
    }
}

fn regression_3n(
    traits: &[f64],
    genotypes: &[f64],
    controls: &[f64],
    diff: bool,
) -> RegResult {
    let mut sig_c = 0.0;
    let mut sig_x = 0.0;
    let mut sig_y = 0.0;
    let mut sig_cc = 0.0;
    let mut sig_xx = 0.0;
    let mut sig_yy = 0.0;
    let mut sig_xc = 0.0;
    let mut sig_cy = 0.0;
    let mut sig_xy = 0.0;

    let n_strains = traits.len();
    let n = n_strains as f64;

    for ix in 0..traits.len() {
        let a = controls[ix];
        let b = genotypes[ix];
        let y = traits[ix];
        sig_c += a;
        sig_x += b;
        sig_y += y;
        sig_cc += a * a;
        sig_xx += b * b;
        sig_yy += y * y;
        sig_xc += a * b;
        sig_cy += y * a;
        sig_xy += y * b;
    }

    let temp0 = sig_xc * sig_xc - sig_cc * sig_xx;
    let temp1 = sig_c * sig_xx - sig_x * sig_xc;
    let temp2 = sig_x * sig_cc - sig_c * sig_xc;
    let temp3 = sig_x * sig_x - n * sig_xx;
    let temp4 = n * sig_xc - sig_c * sig_x;
    let temp5 = sig_c * sig_c - n * sig_cc;
    let temp6 = temp4 * sig_xc + temp2 * sig_x + temp5 * sig_xx;

    let betak = (temp0 * sig_y + temp1 * sig_cy + temp2 * sig_xy) / temp6;
    let mut betac = (temp1 * sig_y + temp3 * sig_cy + temp4 * sig_xy) / temp6;
    let mut betax = (temp2 * sig_y + temp4 * sig_cy + temp5 * sig_xy) / temp6;

    let ssf = sig_yy
        + betac * (betac * sig_cc - 2.0 * sig_cy)
        + betax * (betax * sig_xx - 2.0 * sig_xy)
        + 2.0 * betac * betax * sig_xc
        + betak
            * (n * betak + 2.0 * betac * sig_c + 2.0 * betax * sig_x
                - 2.0 * sig_y);

    let ssr = if diff {
        let d = sig_cc - sig_c * sig_c / n;
        let a = (sig_cc * sig_y - sig_c * sig_cy) / (n * d);
        let b = (sig_cy - (sig_c * sig_y / n)) / d;
        sig_yy
            + a * (n * a - 2.0 * sig_y)
            + b * (2.0 * a * sig_c + b * sig_cc - 2.0 * sig_cy)
    } else {
        sig_yy - (sig_y * sig_y) / n
    };

    let mut lrs = n * (ssr / ssf).ln();
    if lrs.is_nan() || lrs < 0.0 {
        betax = 0.0;
        lrs = 0.0;
        // NOTE: in the old implementation it is `betak`, not `betac`, that is set to 0.0 here, but `betak` is not used later, so I assume it's a mistake!
        betac = 0.0;
    }

    RegResult {
        lrs,
        additive: betax,
        dominance: Some(betac),
    }
}

// this one will require a bit more work since it actually uses matrices!
/*
fn regression_3n(
    traits: &[f64],
    genotypes: &[f64],
    controls: &[f64],
    variance: &[f64],
    diff: bool,
) -> RegResult {


    let mut sig1V = 0.0;
    let mut sigYV = 0.0;
    let mut sigXV =0.0;
    let mut sigCV =0.0;
    let mut sigXXV = 0.0;
    let mut sigYYV = 0.0;
    let mut sigCCV = 0.0;
    let mut sigXYV = 0.0;
    let mut sigXCV = 0.0;
    let mut sigCYV = 0.0;


    let n_strains = traits.len();
    let n = n_strains as f64;

    for ix in 0..traits.len() {
        let c = controls[ix];
        let x = genotypes[ix];
        let y = traits[ix];
        let v = 1.0/variance[ix];
        sig1V += v;
        sigYV += y*v;
        sigXV += x*v;
        sigCV += c*v;
        sigXXV += x*x*v;
        sigYYV += y*y*v;
        sigCCV += c*c*v;
        sigXYV += x*y*v;
        sigXCV += x*c*v;
        sigCYV += c*y*v;
    }

    aa = square(3);
    aa[0][0] = sig1V;
    aa[1][1] = sigXXV;
    aa[2][2] = sigCCV;
    aa[0][1] = aa[1][0] = sigXV;
    aa[0][2] = aa[2][0] = sigCV;
    aa[1][2] = aa[2][1] = sigXCV;

    inverse(aa,3);

    betak = aa[0][0]*sigYV + aa[0][1]*sigXYV + aa[0][2]*sigCYV;
    betax = aa[1][0]*sigYV + aa[1][1]*sigXYV + aa[1][2]*sigCYV;
    betac = aa[2][0]*sigYV + aa[2][1]*sigXYV + aa[2][2]*sigCYV;
    ssf = sigYYV+betax*(betax*sigXXV-2*sigXYV)+betac*(betac*sigCCV-2*sigCYV)+ 2*betax*betac*sigXCV+betak*(sig1V*betak+2*betax*sigXV+2*betac*sigCV-2*sigYV);
    if (diff != 0){
        D = sigCCV - sigCV*sigCV/sig1V;
        a = (sigCCV*sigYV - sigCV*sigCYV)/(sig1V*D);
        b = (sigCYV - (sigCV*sigYV/sig1V))/D;
        ssr = sigYYV + a*(sig1V*a-2.0*sigYV) + b*(2.0*a*sigCV+b*sigCCV-2.0*sigCYV);
    }
    else
        ssr = sigYYV - (sigYV*sigYV)/sig1V;

    LRS = n*log(ssr/ssf);
    if (isnan(LRS) || (LRS < 0)){
        betax = 0.0;
        betac = 0.0;
        LRS = 0.0;
    }
    result[0] = LRS;
    result[1] = betax;
    result[2] = betac;
    freesquare(aa,3);
    return 1;

    RegResult {
        lrs,
        additive: betax,
        dominance: Some(betac),
    }
}

*/
