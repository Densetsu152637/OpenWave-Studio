//! A small radix-2 complex FFT (double precision) for the analysis modules.
//!
//! Written here so `ow-dsp` stays free of FFT dependencies; analysis runs on control threads,
//! so it allocates freely.

use std::f64::consts::PI;

/// A reusable plan for one power-of-two size.
pub(crate) struct Fft {
    size: usize,
    cos: Vec<f64>,
    sin: Vec<f64>,
    reverse: Vec<u32>,
}

impl Fft {
    /// Plan for `size` points; `size` must be a power of two of at least 2.
    pub(crate) fn new(size: usize) -> Self {
        assert!(size.is_power_of_two() && size >= 2);
        let bits = size.trailing_zeros();
        let reverse = (0..size as u32)
            .map(|i| i.reverse_bits() >> (32 - bits))
            .collect();
        let (cos, sin) = (0..size / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / size as f64;
                (a.cos(), a.sin())
            })
            .unzip();
        Self {
            size,
            cos,
            sin,
            reverse,
        }
    }

    /// In-place forward transform of `re`/`im`, both exactly `size` long.
    pub(crate) fn forward(&self, re: &mut [f64], im: &mut [f64]) {
        let n = self.size;
        debug_assert!(re.len() == n && im.len() == n);
        for (i, &reversed) in self.reverse.iter().enumerate().take(n) {
            let j = reversed as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut half = 1;
        while half < n {
            let step = n / (half * 2);
            for start in (0..n).step_by(half * 2) {
                for k in 0..half {
                    let (wr, wi) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            half *= 2;
        }
    }
}

/// Hann window of `n` points (periodic form, for overlap-add analysis).
pub(crate) fn hann(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos())
        .collect()
}

/// The smallest power of two that is at least `n` (and at least 2).
pub(crate) fn next_pow2(n: usize) -> usize {
    n.max(2).next_power_of_two()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_of_a_cosine_has_two_bins() {
        let n = 64;
        let fft = Fft::new(n);
        let mut re: Vec<f64> = (0..n)
            .map(|i| (2.0 * PI * 5.0 * i as f64 / n as f64).cos())
            .collect();
        let mut im = vec![0.0; n];
        fft.forward(&mut re, &mut im);
        for k in 0..n {
            let mag = re[k].hypot(im[k]);
            let want = if k == 5 || k == n - 5 {
                n as f64 / 2.0
            } else {
                0.0
            };
            assert!((mag - want).abs() < 1e-9, "bin {k}: {mag}");
        }
    }
}
