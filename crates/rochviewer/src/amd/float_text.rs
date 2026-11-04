//! A float that prints an unreadable sensor in lower case.
//!
//! Rust writes NaN as `NaN`, which put `NaN °C` on screen where the rest of
//! the readings are lower case. Wrapping the value writes `nan` instead and
//! changes nothing else: every finite value, both infinities and exact ties
//! format exactly as they would unwrapped. Kept in the AMD package because
//! the AMD display paths are what use it.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatText(pub f64);

impl fmt::Display for FloatText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_nan() {
            // write_str, not pad: a precision would cut "nan" short.
            return f.write_str("nan");
        }
        fmt::Display::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nan_is_spelled_as_python_spells_it() {
        // "%.1f" % float("nan") == "nan", for either sign and any precision.
        assert_eq!(format!("{:.1}", FloatText(f64::NAN)), "nan");
        assert_eq!(format!("{:.3}", FloatText(-f64::NAN)), "nan");
        assert_eq!(format!("{:.0} ns", FloatText(f64::NAN)), "nan ns");
        assert_eq!(format!("{:.2}", FloatText(f64::INFINITY)), "inf");
        assert_eq!(format!("{:.2}", FloatText(f64::NEG_INFINITY)), "-inf");
        // Everything else is Rust's own formatting, ties to even included:
        // "%.1f" % 0.25 == "0.2", "%.2f" % -1.005 == "-1.00".
        assert_eq!(format!("{:.1}", FloatText(0.25)), "0.2");
        assert_eq!(format!("{:.2}", FloatText(-1.005)), "-1.00");
    }
}
