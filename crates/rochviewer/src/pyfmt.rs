//! Python-compatible text formatting.
//!
//! Display strings are compared against what the Python version printed, so
//! the few places Rust's defaults differ are written out here once:
//! `str(float)` keeps a trailing `.0` and switches to exponent form outside
//! [1e-4, 1e16), and `round()` rounds half to even.

/// Python's `str(x)` / `repr(x)` for a float.
pub fn float_str(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let abs = value.abs();
    if (1e-4..1e16).contains(&abs) {
        // Rust's Display is shortest-round-trip like Python's repr, but drops
        // the ".0" of a whole number.
        let text = format!("{value}");
        if text.contains('.') { text } else { format!("{text}.0") }
    } else {
        let text = format!("{value:e}");
        let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
        let exp: i32 = exponent.parse().unwrap_or(0);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    }
}

/// Python's `round(x, ndigits)` for a float: half to even on the scaled value.
pub fn round_half_even(value: f64, ndigits: i32) -> f64 {
    let factor = 10f64.powi(ndigits);
    let scaled = value * factor;
    let result = if (scaled - scaled.trunc()).abs() == 0.5 {
        2.0 * (scaled / 2.0).round()
    } else {
        scaled.round()
    };
    result / factor
}

/// Python's `round(x)` with no digits: an integer, half to even.
pub fn round_int(value: f64) -> i64 {
    round_half_even(value, 0) as i64
}

/// Python's `"%.Nf" % x`. Both languages format the exact binary value, so
/// this is Rust's `{:.N}`; it exists so ported code reads like the original.
pub fn fixed(value: f64, decimals: usize) -> String {
    format!("{value:.decimals$}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_match_python_str() {
        assert_eq!(float_str(1.0), "1.0");
        assert_eq!(float_str(1.25), "1.25");
        assert_eq!(float_str(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(float_str(133.334), "133.334");
        assert_eq!(float_str(1e16), "1e+16");
        assert_eq!(float_str(1.5e-5), "1.5e-05");
        assert_eq!(float_str(-2.0), "-2.0");
    }

    #[test]
    fn round_is_half_even() {
        assert_eq!(round_half_even(2.5, 0), 2.0);
        assert_eq!(round_half_even(3.5, 0), 4.0);
        assert_eq!(round_half_even(1.234, 2), 1.23);
        assert_eq!(round_int(0.5), 0);
        assert_eq!(round_int(1.5), 2);
    }

    #[test]
    fn fixed_matches_percent_f() {
        assert_eq!(fixed(1.2345, 3), "1.234");
        assert_eq!(fixed(1.0, 3), "1.000");
    }
}
