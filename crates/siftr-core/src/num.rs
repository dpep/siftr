//! Rounding to the precision a number actually has.

/// Rounds `value` to `digits` significant figures.
pub fn round_sig(value: f64, digits: i32) -> f64 {
    if value == 0.0 || !value.is_finite() {
        return value;
    }
    let exponent = digits - 1 - value.abs().log10().floor() as i32;
    // Divide by an exact power of ten rather than multiply by an inexact 0.1, 0.01, …
    if exponent >= 0 {
        let scale = 10f64.powi(exponent);
        (value * scale).round() / scale
    } else {
        let scale = 10f64.powi(-exponent);
        (value / scale).round() * scale
    }
}

/// The middle value, averaging the two middle ones for an even count. `None` when empty.
pub fn median(values: &[f64]) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    match sorted.len() {
        0 => None,
        len if len % 2 == 1 => Some(sorted[mid]),
        _ => Some((sorted[mid - 1] + sorted[mid]) / 2.0),
    }
}

/// Median absolute deviation from the median. `None` when empty.
pub fn mad(values: &[f64]) -> Option<f64> {
    let m = median(values)?;
    median(&values.iter().map(|v| (v - m).abs()).collect::<Vec<_>>())
}

/// Rounds `value` to `digits` significant figures, half up.
pub fn round_sig_u64(value: u64, digits: u32) -> u64 {
    let places = value.checked_ilog10().unwrap_or(0) + 1;
    if places <= digits {
        return value;
    }
    let scale = 10u64.pow(places - digits);
    value.saturating_add(scale / 2) / scale * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_floats_to_significant_figures() {
        let cases = [
            (0.666_666, 2, 0.67),
            (1234.5, 3, 1230.0),
            (0.000_123_45, 2, 0.000_12),
            (5.0, 3, 5.0),
            (0.0, 2, 0.0),
        ];
        for (value, digits, expected) in cases {
            assert_eq!(round_sig(value, digits), expected, "{value} to {digits}");
        }
    }

    #[test]
    fn medians_and_deviations() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[1.0, 1.1]), Some(1.05));
        assert_eq!(mad(&[100.0, 105.0, 110.0]), Some(5.0));
    }

    #[test]
    fn rounds_integers_to_significant_figures() {
        let cases = [
            (12_345, 2, 12_000),
            (15_000, 1, 20_000),
            (99, 2, 99),
            (0, 2, 0),
        ];
        for (value, digits, expected) in cases {
            assert_eq!(
                round_sig_u64(value, digits),
                expected,
                "{value} to {digits}"
            );
        }
    }
}
