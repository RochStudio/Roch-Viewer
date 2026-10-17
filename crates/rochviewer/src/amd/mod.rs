//! The AMD AM5 backend: UMC timings, the APOB, the SMU and the FCH bus.
//!
//! Parts of this package follow ZenStates-Core and ZenTimings by irusanov,
//! both GPL-3.0. Where a register map, a bit field or a bound comes from
//! that work it is credited in the module that uses it. See
//! THIRD_PARTY_NOTICES.txt.

pub mod adl;
pub mod adlx;
pub mod agesa;
pub mod apob;
pub mod fch_smbus;
pub mod power_metrics;
pub mod profile;
pub mod smn;
pub mod smn_mcfg;
pub mod smu_clocks;
pub mod smu_power;
pub mod smu_voltages;
pub mod timings;

#[cfg(test)]
mod decoder_parity;
