//! How this viewer writes numbers.
//!
//! Rust's own defaults differ from the format the readings are published in,
//! so the rules are written out here once: a float keeps a trailing `.0` on a
//! whole number and switches to exponent form outside [1e-4, 1e16), and
//! rounding is half to even on the value's exact binary form.

/// A float as this viewer prints it: the shortest digits that read back as
/// the same value, a trailing `.0` on a whole number, and exponent form
/// outside [1e-4, 1e16).
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
    let exp10 = format!("{abs:e}").split_once('e').and_then(|(_, e)| e.parse::<i32>().ok()).unwrap_or(0);
    if (1e-4..1e16).contains(&abs) {
        // Rust's Display already picks the shortest round-tripping digits,
        // but drops the ".0" of a whole number.
        let mut text = format!("{value}");
        if significant_digits(&text) == 17 {
            // When the shortest form takes all 17 digits and the true value
            // sits exactly between two of them, the even one is wanted
            // (191966.68188476562); Rust's shortest form picks the other.
            // Fixed-precision formatting rounds the exact value half to
            // even, so the digits come from there.
            let decimals = (16 - exp10).max(0) as usize;
            text = format!("{value:.decimals$}");
        }
        if text.contains('.') { text } else { format!("{text}.0") }
    } else {
        let mut text = format!("{value:e}");
        if significant_digits(text.split_once('e').map_or(text.as_str(), |(m, _)| m)) == 17 {
            text = format!("{value:.16e}");
        }
        let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
        let exp: i32 = exponent.parse().unwrap_or(0);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    }
}

/// Digits in a decimal string from the first non-zero one, sign and point
/// aside.
fn significant_digits(text: &str) -> usize {
    text.chars()
        .filter(char::is_ascii_digit)
        .skip_while(|c| *c == '0')
        .count()
}

/// Round a float to `ndigits`, half to even.
///
/// The rounding is of the value's exact binary form, so half to even applies
/// only on a true tie: 99.65 is stored a hair above 99.65 and so becomes
/// 99.7. Scaling by ten first (996.5, an exact tie after the multiply) and
/// rounding that gave 99.6 -- which is how BCLK 99.65 MHz once made DDR5-7976
/// read 7968. Rust's fixed-precision formatting works from the exact value
/// with the same tie rule, so the digits come from there.
pub fn round_half_even(value: f64, ndigits: i32) -> f64 {
    if !value.is_finite() {
        return value;
    }
    if ndigits >= 0 {
        let digits = ndigits as usize;
        return format!("{value:.digits$}").parse().unwrap_or(value);
    }
    // Negative ndigits (round to tens, hundreds): scale down, round, scale up.
    let factor = 10f64.powi(-ndigits);
    let scaled = value / factor;
    let result = if (scaled - scaled.trunc()).abs() == 0.5 { 2.0 * (scaled / 2.0).round() } else { scaled.round() };
    result * factor
}

/// Round to a whole number, half to even.
pub fn round_int(value: f64) -> i64 {
    round_half_even(value, 0) as i64
}

/// A float to a fixed number of decimals.
///
/// Rust's `{:.N}` formats the exact binary value, which is what the readings
/// want; this names the operation where it is used as a value rather than in
/// a format string.
pub fn fixed(value: f64, decimals: usize) -> String {
    format!("{value:.decimals$}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_print_in_the_viewers_format() {
        assert_eq!(float_str(1.0), "1.0");
        assert_eq!(float_str(1.25), "1.25");
        assert_eq!(float_str(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(float_str(133.334), "133.334");
        assert_eq!(float_str(1e16), "1e+16");
        assert_eq!(float_str(1.5e-5), "1.5e-05");
        assert_eq!(float_str(-2.0), "-2.0");
        // Seventeen digits on an exact tie: the even one is wanted.
        assert_eq!(float_str(191966.681884765625), "191966.68188476562");
        assert_eq!(float_str(123456.789012345678), "123456.78901234567");
        assert_eq!(float_str(1e15 + 0.3), "1000000000000000.2");
        assert_eq!(float_str(1.0 / 3.0), "0.3333333333333333");
        assert_eq!(float_str(2.0 / 3.0), "0.6666666666666666");
        assert_eq!(float_str(5e-324 * 3.0), "1.5e-323");
    }

    #[test]
    fn round_is_half_even() {
        assert_eq!(round_half_even(2.5, 0), 2.0);
        assert_eq!(round_half_even(3.5, 0), 4.0);
        assert_eq!(round_half_even(1.234, 2), 1.23);
        // What the stored value actually is decides these: 99.65 sits above
        // the tie and gives 99.7, 2.675 sits below and gives 2.67, and 0.125
        // is a true tie, so half to even gives 0.12.
        assert_eq!(round_half_even(99.65, 1), 99.7);
        assert_eq!(round_half_even(2.675, 2), 2.67);
        assert_eq!(round_half_even(0.125, 2), 0.12);
        assert_eq!(round_half_even(972.5435, 3), 972.543);
        assert_eq!(round_half_even(1250.0, -2), 1200.0);
        assert_eq!(round_int(0.5), 0);
        assert_eq!(round_int(1.5), 2);
    }

    #[test]
    fn fixed_matches_percent_f() {
        assert_eq!(fixed(1.2345, 3), "1.234");
        assert_eq!(fixed(1.0, 3), "1.000");
    }
}
