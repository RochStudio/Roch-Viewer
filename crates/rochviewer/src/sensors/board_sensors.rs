//! Port of rochviewer/sensors/board_sensors.py -- not yet ported.
//!
//! The function below is a contract other modules build against; its
//! signature must be kept when the module is ported.

/// The Super I/O chip the board-sensor profile detected, as
/// `(reader module, chip name)` -- the module being the last component of
/// the Python reader's module ("superio_lpc", "nct679x" or "ite_superio"),
/// which `system_identity::lpcio_name` maps to a vendor. `None` when no
/// profile matched or its reader names no chip.
pub fn detected_lpcio_chip() -> Option<(&'static str, String)> {
    None
}
