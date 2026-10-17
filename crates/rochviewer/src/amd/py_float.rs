//! A float that formats the way Python's `%` operator does.
//!
//! `{:.3}` of `PyFloat(x)` is `"%.3f" % x`. Rust's own formatting agrees on
//! every finite value, on both infinities and on exact ties (both round the
//! exact binary value, half to even), but writes NaN as `NaN` where Python
//! writes `nan` -- so `"%.1f °C" % nan` came out of the port as `NaN °C`.
//! Kept in the AMD package because the AMD display paths are what use it.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PyFloat(pub f64);

impl fmt::Display for PyFloat {
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
        assert_eq!(format!("{:.1}", PyFloat(f64::NAN)), "nan");
        assert_eq!(format!("{:.3}", PyFloat(-f64::NAN)), "nan");
        assert_eq!(format!("{:.0} ns", PyFloat(f64::NAN)), "nan ns");
        assert_eq!(format!("{:.2}", PyFloat(f64::INFINITY)), "inf");
        assert_eq!(format!("{:.2}", PyFloat(f64::NEG_INFINITY)), "-inf");
        // Everything else is Rust's own formatting, ties to even included:
        // "%.1f" % 0.25 == "0.2", "%.2f" % -1.005 == "-1.00".
        assert_eq!(format!("{:.1}", PyFloat(0.25)), "0.2");
        assert_eq!(format!("{:.2}", PyFloat(-1.005)), "-1.00");
    }
}
