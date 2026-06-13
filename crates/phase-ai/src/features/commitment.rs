//! Shared deck-feature commitment formulas.
//!
//! Feature commitment is calibrated on a per-60-nonland equivalent density so
//! 60-card and 99-card decks with the same nonland composition produce the
//! same activation strength.

/// Convert a raw count into a per-60-nonland equivalent density.
pub fn density_per_60(count: u32, total_nonland: u32) -> f32 {
    if total_nonland == 0 {
        0.0
    } else {
        count as f32 / total_nonland as f32 * 60.0
    }
}

/// Weighted-sum commitment shape for features where missing pillars are
/// tolerable. Inputs are `(weight, per_60_density)` pairs.
pub fn weighted_sum(pillars: &[(f32, f32)]) -> f32 {
    pillars
        .iter()
        .map(|(weight, density)| weight * density)
        .sum::<f32>()
        .clamp(0.0, 1.0)
}

/// Geometric-mean commitment shape for features where every pillar must be
/// present. Any zero pillar collapses to `0.0`.
pub fn geometric_mean(pillars: &[f32]) -> f32 {
    if pillars.is_empty() || pillars.iter().any(|density| *density <= 0.0) {
        return 0.0;
    }

    pillars
        .iter()
        .product::<f32>()
        .powf(1.0 / pillars.len() as f32)
        .clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_per_60_is_format_size_neutral() {
        assert!((density_per_60(6, 60) - density_per_60(10, 100)).abs() < 1e-5);
    }

    #[test]
    fn weighted_sum_clamps() {
        assert_eq!(weighted_sum(&[(0.5, 3.0)]), 1.0);
    }

    #[test]
    fn geometric_mean_collapses_missing_pillar() {
        assert_eq!(geometric_mean(&[0.5, 0.0, 0.5]), 0.0);
    }

    #[test]
    fn geometric_mean_clamps() {
        assert_eq!(geometric_mean(&[2.0, 2.0]), 1.0);
    }
}
