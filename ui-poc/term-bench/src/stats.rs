//! Tiny statistics helpers.

pub fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        f64::NAN
    } else if !n.is_multiple_of(2) {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Summary {
    pub mean_us: f64,
    pub p99_us: f64,
}

impl Summary {
    pub fn of(samples_us: &mut [f64]) -> Self {
        samples_us.sort_by(f64::total_cmp);
        let mean_us = samples_us.iter().sum::<f64>() / samples_us.len() as f64;
        let idx = ((samples_us.len() as f64 * 0.99).ceil() as usize).saturating_sub(1);
        Self {
            mean_us,
            p99_us: samples_us[idx],
        }
    }

    /// Median (per field) across repeated runs.
    pub fn median_of(runs: &[Summary]) -> Self {
        let mut means: Vec<f64> = runs.iter().map(|s| s.mean_us).collect();
        let mut p99s: Vec<f64> = runs.iter().map(|s| s.p99_us).collect();
        Self {
            mean_us: median(&mut means),
            p99_us: median(&mut p99s),
        }
    }
}
