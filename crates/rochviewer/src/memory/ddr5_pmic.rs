// This file follows ZenStates-Core and ZenTimings by irusanov
// (https://github.com/irusanov), both GPL-3.0. Register numbers, bit fields
// and the bounds applied to decoded values were taken from or checked against
// that work, and the comments below say where. Copyright in those parts
// remains with their authors; this file is distributed under the same licence
// they are, which is what makes that use permitted.

//! DDR5 PMIC rail decoding (JESD301 layout).
//!
//! The DIMM's PMIC owns VDD, VDDQ and VPP.  The CPU SMU does not report them —
//! not reachable any other way on this platform -- so these registers are the
//! only source.
//!
//! Register and encoding constants below are the JEDEC-documented layout, but
//! whether this board's PMIC answers with them is a hardware question:
//! [`CONFIRMED_PMIC_RAILS`] stays empty until `ddr5_pmic_probe` has matched
//! the decoded values against a known-good monitor on real hardware.  Same
//! evidence rule as the PM-table offsets, for the same reason — a wrong decode
//! prints a believable but false voltage.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::ddr5_telemetry::{ReaderFactory, SmbusError, SmbusReader, default_smbus_backend};
use crate::memo::Memo;
// Believable ranges live with the rail definitions, so the transport and the
// view can never disagree about what counts as a plausible reading.
use crate::sensors::voltage_rails::{rail_by_key, validate_voltage};

// VID (voltage-ID) registers — the configured rail setting.
// Register numbers confirmed on MSI B850MPOWER + G.Skill F5-6000J2636G16G by
// matching HWiNFO at two profiles (see CONFIRMED_PMIC_RAILS); formulas from
// the ZenStates-Core Ddr5PmicDecoder reference implementation.
pub const SWA_VDD_REGISTER: u8 = 0x21; // SWA -> VDD
pub const SWB_VDDQ_REGISTER: u8 = 0x25; // SWB -> VDDQ
pub const SWC_VPP_REGISTER: u8 = 0x27; // SWC -> VPP

// JESD301 VID encoding.  SWA/SWB sit above a 800 mV floor, SWC above 1500 mV,
// both in 5 mV steps.  Two modes exist and the register itself does not say
// which is active:
//   7-bit JEDEC : mV = base + ((reg >> 1) & 0x7F) * 5
//   8-bit OC    : mV = base + reg * 5          (vendor overclocking extension)
pub const SWA_SWB_BASE_MV: u32 = 800;
pub const SWC_BASE_MV: u32 = 1500;
pub const VID_STEP_MV: u32 = 5;

// Mode register.  The VID registers do not record whether the vendor 8-bit
// overclocking extension is active, so it is read from here.
//
// Confirmed by capturing at JEDEC 4800 and again at 8200 MT/s: R2Bh moved
// 0x42 -> 0x72, i.e. bits 4 and 5 set, exactly when SWA and SWB switched from
// the 7-bit to the 8-bit decode.  SWC stayed 7-bit at both profiles.
pub const VID_MODE_REGISTER: u8 = 0x2B;
pub const VID_MODE_SWA_8BIT: u8 = 1 << 4;
pub const VID_MODE_SWB_8BIT: u8 = 1 << 5;

/// One confirmed rail: Python's `key -> (VID register, base mV, 8-bit mode
/// mask or None for always 7-bit)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PmicRailSpec {
    pub key: &'static str,
    pub register: u8,
    pub base_mv: u32,
    pub mode_mask: Option<u8>,
}

// rail key -> (VID register, base mV, 8-bit mode mask or None for always 7-bit)
//
// Confirmed on MSI B850MPOWER + Ryzen 7 9850X3D + G.Skill F5-6000J2636G16G,
// PMIC 0x49/0x4B, against HWiNFO 8.44 and ZenTimings 1.43.0 at TWO profiles:
//
//                     JEDEC 4800            8200 MT/s
//   R21h SWA   0x78 -> 1.100 V (7-bit)  0x8C -> 1.500 V (8-bit)   HWiNFO 1.500
//   R25h SWB   0x78 -> 1.100 V (7-bit)  0x80 -> 1.440 V (8-bit)   ZenT  1.4400
//   R27h SWC   0x78 -> 1.800 V (7-bit)  0x78 -> 1.800 V (7-bit)   both  1.800
//
// Formulas from the ZenStates-Core Ddr5PmicDecoder reference implementation:
//   7-bit JEDEC : mV = base + ((reg >> 1) & 0x7F) * 5
//   8-bit OC    : mV = base + reg * 5
pub const CONFIRMED_PMIC_RAILS: &[PmicRailSpec] = &[
    PmicRailSpec {
        key: "dram_vdd",
        register: SWA_VDD_REGISTER,
        base_mv: SWA_SWB_BASE_MV,
        mode_mask: Some(VID_MODE_SWA_8BIT),
    },
    PmicRailSpec {
        key: "dram_vddq",
        register: SWB_VDDQ_REGISTER,
        base_mv: SWA_SWB_BASE_MV,
        mode_mask: Some(VID_MODE_SWB_8BIT),
    },
    PmicRailSpec { key: "dram_vpp", register: SWC_VPP_REGISTER, base_mv: SWC_BASE_MV, mode_mask: None },
];

/// `{rail key: volts}`.
pub type PmicRails = BTreeMap<&'static str, f64>;

/// Decode the confirmed DRAM rails from one PMIC.
///
/// `read_register(register)` returns a byte.  Rails that decode outside
/// their believable range are dropped rather than displayed. A register that
/// does not read (refused, timed out, bus error) is skipped; anything else a
/// reader fails with is passed on, as the Python let it escape.
pub fn decode_rails(
    read_register: &dyn Fn(u8) -> Result<u8, SmbusError>,
    rails: Option<&[PmicRailSpec]>,
) -> Result<PmicRails, SmbusError> {
    let rails = rails.unwrap_or(CONFIRMED_PMIC_RAILS);
    let mode = match read_register(VID_MODE_REGISTER) {
        Ok(mode) => mode,
        Err(error) if error.is_transfer() => return Ok(PmicRails::new()),
        Err(error) => return Err(error),
    };
    let mut values = PmicRails::new();
    for spec in rails {
        let raw = match read_register(spec.register) {
            Ok(raw) => raw,
            Err(error) if error.is_transfer() => continue,
            Err(error) => return Err(error),
        };
        let eight_bit = spec.mode_mask.is_some_and(|mask| mode & mask != 0);
        let volts = if eight_bit { decode_vid_8bit(raw, spec.base_mv) } else { decode_vid_7bit(raw, spec.base_mv) };
        let Some(rail) = rail_by_key(spec.key) else {
            continue;
        };
        if let Ok(volts) = validate_voltage(rail, volts) {
            values.insert(spec.key, volts);
        }
    }
    Ok(values)
}

// Which PMIC answered last time, so repeat reads skip the bus scan.
//
// This matters for responsiveness: a probe of an empty address blocks for the
// full SMBus timeout, and scanning both controllers across all eight PMIC
// addresses is mostly misses. Rediscovering that on every refresh made the UI
// stutter. The location cannot change while the machine is running.
#[derive(Clone)]
struct PmicLocation {
    reader: Arc<dyn SmbusReader>,
    controller: u8,
    address: u8,
}

static PMIC_LOCATION: Memo<Option<PmicLocation>> = Memo::new();

fn rails_from(reader: &dyn SmbusReader, address: u8, controller: u8) -> Result<PmicRails, SmbusError> {
    decode_rails(&|register| reader.read_byte(address, register, controller), None)
}

/// Read DRAM VDD/VDDQ/VPP from the first responding DIMM PMIC.
///
/// Returns `{rail key: volts}`; empty when no PMIC answers.  Both DIMMs are
/// driven from the same board setting, so the first one that responds is
/// representative.
///
/// The decode is JEDEC and the same on any platform; only the bus underneath
/// differs.  An Intel caller passes `intel_pch_smbus`'s reader along with its
/// own controller and address lists, and nothing here changes.  Left out,
/// they are the platform's (see [`default_smbus_backend`]).
pub fn read_dram_rails(
    reader_factory: Option<ReaderFactory>,
    controllers: Option<&[u8]>,
    addresses: Option<&[u8]>,
) -> PmicRails {
    if CONFIRMED_PMIC_RAILS.is_empty() {
        return PmicRails::new();
    }
    let run = || -> Result<PmicRails, SmbusError> {
        let mut reader_factory = reader_factory;
        let mut controllers = controllers;
        let mut addresses = addresses;
        let backend;
        // The host controller differs per platform and the PMIC does not, so
        // the transport comes from the dispatcher rather than being named
        // here. It used to be hardcoded -- to the AMD one while this was an
        // AM5 tool, to the Intel one while the tree was Intel-only -- and
        // either way it was right by accident, because every caller in the
        // app passes a factory in.
        if reader_factory.is_none() || controllers.is_none() || addresses.is_none() {
            backend = default_smbus_backend();
            let (offsets, pmics): (&[u8], &[u8]) = match &backend {
                None if reader_factory.is_none() => return Ok(PmicRails::new()),
                None => {
                    // A caller that supplied a reader supplied the transport.
                    // Only the addresses are missing, and those are JEDEC's --
                    // PMICs answer at 0x48-0x4F behind any host controller.
                    // Bailing here regardless discarded the caller's reader,
                    // which is the same mistake read_identity used to make.
                    use crate::intel::intel_pch_smbus::{CONTROLLER_OFFSETS, PMIC_ADDRESSES};
                    (&CONTROLLER_OFFSETS, &PMIC_ADDRESSES)
                }
                Some(found) => {
                    if reader_factory.is_none() {
                        reader_factory = Some(&found.factory);
                    }
                    (found.controllers, found.pmics)
                }
            };
            controllers = controllers.or(Some(offsets));
            addresses = addresses.or(Some(pmics));
        }
        let (Some(reader_factory), Some(controllers), Some(addresses)) = (reader_factory, controllers, addresses)
        else {
            return Ok(PmicRails::new());
        };

        if let Some(cached) = PMIC_LOCATION.get().flatten() {
            let values = rails_from(cached.reader.as_ref(), cached.address, cached.controller)?;
            if !values.is_empty() {
                return Ok(values);
            }
            // The DIMM stopped answering; fall through and rescan once.
            PMIC_LOCATION.set(None);
        }

        let reader = reader_factory()?;
        if !reader.is_driver_open() {
            return Ok(PmicRails::new());
        }
        for &controller in controllers {
            for &address in addresses {
                let values = rails_from(reader.as_ref(), address, controller)?;
                if !values.is_empty() {
                    PMIC_LOCATION.set(Some(PmicLocation { reader: reader.clone(), controller, address }));
                    return Ok(values);
                }
            }
        }
        Ok(PmicRails::new())
    };
    match run() {
        Ok(values) => values,
        Err(_) => {
            PMIC_LOCATION.set(None);
            PmicRails::new()
        }
    }
}

// SPD5 hub temperature sensor. MR49/MR50 hold a 16-bit reading whose bits
// [12:2] are an 11-bit signed value at 0.25 C per LSB. Register numbers and the
// conversion follow the ZenStates-Core Ddr5ThermalSensor reference.
pub const SPD_TEMPERATURE_REGISTER: u8 = 0x31; // MR49, low byte; MR50 follows at 0x32
pub const SPD_TEMPERATURE_STEP_C: f64 = 0.25;
pub const SPD_HUB_BASE_ADDRESS: u8 = 0x50; // first slot of channel A
pub const DIMM_TEMPERATURE_MIN_C: f64 = 0.0;
pub const DIMM_TEMPERATURE_MAX_C: f64 = 125.0;

// SPD5 hub identity, MR0/MR1. An SPD5118-class hub answers 0x51/0x18 there,
// confirmed on the Z790 bench where both populated slots reported exactly that.
//
// Asked before the temperature is believed, and the reason is specific: a DDR4
// module's SPD EEPROM lives at these same 0x50-0x57 addresses. It answers a
// byte read perfectly happily, and MR49/MR50 would then land on two ordinary
// SPD bytes whose value decodes to a number the band below cannot reject --
// a DDR4 board reporting a confident, invented DIMM temperature. Two reads
// settle what the device actually is, which is the difference between a
// reading and a coincidence.
pub const SPD_HUB_DEVICE_TYPE_REGISTER: u8 = 0x00; // MR0; MR1 follows at 0x01
pub const SPD_HUB_DEVICE_TYPE: (u8, u8) = (0x51, 0x18);

/// True when the device at this address identifies as an SPD5 hub.
///
/// A register that does not read means "no"; any other failure is passed on.
pub fn is_spd5_hub(read_register: &dyn Fn(u8) -> Result<u8, SmbusError>) -> Result<bool, SmbusError> {
    let read = || -> Result<(u8, u8), SmbusError> {
        Ok((read_register(SPD_HUB_DEVICE_TYPE_REGISTER)?, read_register(SPD_HUB_DEVICE_TYPE_REGISTER + 1)?))
    };
    match read() {
        Ok(pair) => Ok(pair == SPD_HUB_DEVICE_TYPE),
        Err(error) if error.is_transfer() => Ok(false),
        Err(error) => Err(error),
    }
}

/// Decode the MR49/MR50 pair into degrees Celsius.
pub fn decode_spd_temperature(low: u8, high: u8) -> f64 {
    let raw = ((high as i32) << 8) | low as i32;
    let mut value = (raw >> 2) & 0x7FF;
    if value & 0x400 != 0 {
        // 11-bit sign bit
        value -= 0x800;
    }
    value as f64 * SPD_TEMPERATURE_STEP_C
}

/// The channel an SPD hub address belongs to.
///
/// DDR5 slots take one SMBus address each, in slot order: 0x50 and 0x51 are
/// the two channel-A slots, 0x52 and 0x53 the two channel-B slots. The bench
/// board answers on 0x51 and 0x53, one per channel, which is what a
/// two-DIMM AM5 build looks like.
pub fn spd_hub_channel(address: u8) -> Option<&'static str> {
    let index = (address as i32 - SPD_HUB_BASE_ADDRESS as i32).div_euclid(2);
    match index {
        0 => Some("a"),
        1 => Some("b"),
        _ => None,
    }
}

// Which SPD hubs answered last, so repeat reads skip the scan.
#[derive(Clone, Default)]
struct SpdLocations {
    reader: Option<Arc<dyn SmbusReader>>,
    locations: Vec<(u8, u8)>,
}

static SPD_LOCATIONS: Memo<SpdLocations> = Memo::new();

/// `{channel: celsius}`, keyed "a"/"b".
pub type DimmTemperatures = BTreeMap<&'static str, f64>;

/// Read every DIMM's own SPD hub temperature sensor.
///
/// The only readings in this project that come from the modules rather than
/// the CPU or the board. Returns `{channel: celsius}` keyed "a"/"b", and an
/// empty map when no hub answers.
///
/// Every populated hub is read, not just the first one to reply: two DIMMs
/// sit in different airflow and routinely differ by a couple of degrees, so
/// one number cannot stand for both.
///
/// An address is only believed once the device there identifies as an SPD5
/// hub; see SPD_HUB_DEVICE_TYPE. The check runs during discovery only, since
/// a module cannot be swapped while the machine is running and repeating it
/// on every refresh would double the bus traffic behind these two rows.
///
/// As with [`read_dram_rails`], the bus is the caller's choice and the
/// dispatcher's default: the host controller differs per platform, and the
/// JESD300-5 decode below does not.
pub fn read_dimm_temperatures(
    reader_factory: Option<ReaderFactory>,
    controllers: Option<&[u8]>,
    addresses: Option<&[u8]>,
) -> DimmTemperatures {
    let mut temperatures = DimmTemperatures::new();
    let mut run = || -> Result<(), SmbusError> {
        let mut reader_factory = reader_factory;
        let mut controllers = controllers;
        let mut addresses = addresses;
        let backend;
        // Same as read_dram_rails: the dispatcher names the transport, since
        // the host controller is the platform's and the hub is not.
        if reader_factory.is_none() || controllers.is_none() || addresses.is_none() {
            backend = default_smbus_backend();
            let (offsets, hubs): (&[u8], &[u8]) = match &backend {
                None if reader_factory.is_none() => return Ok(()),
                None => {
                    // See read_dram_rails: hubs answer at 0x50-0x57 on any
                    // host controller, so a supplied reader is enough.
                    use crate::intel::intel_pch_smbus::{CONTROLLER_OFFSETS, SPD_HUB_ADDRESSES};
                    (&CONTROLLER_OFFSETS, &SPD_HUB_ADDRESSES)
                }
                Some(found) => {
                    if reader_factory.is_none() {
                        reader_factory = Some(&found.factory);
                    }
                    (found.controllers, found.hubs)
                }
            };
            controllers = controllers.or(Some(offsets));
            addresses = addresses.or(Some(hubs));
        }
        let (Some(reader_factory), Some(controllers), Some(addresses)) = (reader_factory, controllers, addresses)
        else {
            return Ok(());
        };

        let cached = SPD_LOCATIONS.get().unwrap_or_default();
        let known = cached.reader.is_some() && !cached.locations.is_empty();
        let candidates: Vec<(u8, u8)> = if known {
            cached.locations.clone()
        } else {
            controllers.iter().flat_map(|&c| addresses.iter().map(move |&a| (c, a))).collect()
        };
        let reader = match cached.reader {
            Some(reader) => reader,
            None => reader_factory()?,
        };
        if !reader.is_driver_open() {
            return Ok(());
        }
        let mut found = Vec::new();
        for (controller, address) in candidates {
            let Some(channel) = spd_hub_channel(address) else {
                continue;
            };
            if temperatures.contains_key(channel) {
                continue;
            }
            if !known && !is_spd5_hub(&|register| reader.read_byte(address, register, controller))? {
                // Something answered, but it is not an SPD5 hub. On a DDR4
                // board that is the module's SPD EEPROM.
                continue;
            }
            let read = || -> Result<(u8, u8), SmbusError> {
                Ok((
                    reader.read_byte(address, SPD_TEMPERATURE_REGISTER, controller)?,
                    reader.read_byte(address, SPD_TEMPERATURE_REGISTER + 1, controller)?,
                ))
            };
            let (low, high) = match read() {
                Ok(pair) => pair,
                Err(error) if error.is_transfer() => continue,
                Err(error) => return Err(error),
            };
            let celsius = decode_spd_temperature(low, high);
            if (DIMM_TEMPERATURE_MIN_C..=DIMM_TEMPERATURE_MAX_C).contains(&celsius) {
                temperatures.insert(channel, celsius);
                found.push((controller, address));
            }
        }
        if !found.is_empty() {
            SPD_LOCATIONS.set(SpdLocations { reader: Some(reader), locations: found });
        } else {
            SPD_LOCATIONS.set(SpdLocations::default());
        }
        Ok(())
    };
    match run() {
        Ok(()) => temperatures,
        Err(_) => {
            SPD_LOCATIONS.set(SpdLocations::default());
            DimmTemperatures::new()
        }
    }
}

/// Warmest populated DIMM, or `None` when no hub answers.
///
/// Kept for callers that want one number for the set; the per-DIMM rows use
/// [`read_dimm_temperatures`].
pub fn read_dimm_temperature(reader_factory: Option<ReaderFactory>) -> Option<f64> {
    read_dimm_temperatures(reader_factory, None, None).into_values().reduce(f64::max)
}

/// JEDEC 7-bit VID decode, in volts.
pub fn decode_vid_7bit(raw: u8, base_mv: u32) -> f64 {
    (base_mv + ((raw as u32 >> 1) & 0x7F) * VID_STEP_MV) as f64 / 1000.0
}

/// Vendor 8-bit overclocking VID decode, in volts.
pub fn decode_vid_8bit(raw: u8, base_mv: u32) -> f64 {
    (base_mv + raw as u32 * VID_STEP_MV) as f64 / 1000.0
}

// The PMIC's ADC telemetry window is deliberately not used. It was explored as
// a way to break the 7-bit/8-bit VID tie, but VID_MODE_REGISTER answers that
// directly and carries two-profile evidence. Dropping it also leaves the SPD
// page selector as the only writable SMBus target, so no PMIC register is
// writable at all.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // --- Ddr5PmicDecodeTest (tests/test_amd_fch_smbus.py)

    fn table_reader(table: BTreeMap<u8, u8>) -> impl Fn(u8) -> Result<u8, SmbusError> {
        move |register| table.get(&register).copied().ok_or_else(|| SmbusError::Other(format!("KeyError: {register}")))
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.5e-4
    }

    #[test]
    fn confirmed_rails_use_the_vid_registers() {
        let registers: BTreeMap<&str, u8> = CONFIRMED_PMIC_RAILS.iter().map(|r| (r.key, r.register)).collect();
        assert_eq!(registers, BTreeMap::from([("dram_vdd", 0x21), ("dram_vddq", 0x25), ("dram_vpp", 0x27)]));
    }

    #[test]
    fn captured_registers_decode_to_the_hwinfo_readings() {
        // 8200 MT/s: R2Bh = 0x72 puts SWA and SWB into 8-bit mode.
        let table = BTreeMap::from([(0x2B, 0x72), (0x21, 0x8C), (0x25, 0x80), (0x27, 0x78)]);
        let rails = decode_rails(&table_reader(table), None).unwrap();
        assert!(close(rails["dram_vdd"], 1.500));
        assert!(close(rails["dram_vddq"], 1.440));
        assert!(close(rails["dram_vpp"], 1.800));
    }

    #[test]
    fn same_registers_decode_correctly_at_jedec() {
        // JEDEC 4800: R2Bh = 0x42, so SWA/SWB fall back to the 7-bit decode.
        // The identical VID byte 0x78 means 1.100 V here and 1.400 V in 8-bit
        // mode, which is exactly why the mode register has to be consulted.
        let table = BTreeMap::from([(0x2B, 0x42), (0x21, 0x78), (0x25, 0x78), (0x27, 0x78)]);
        let rails = decode_rails(&table_reader(table), None).unwrap();
        assert!(close(rails["dram_vdd"], 1.100));
        assert!(close(rails["dram_vddq"], 1.100));
        assert!(close(rails["dram_vpp"], 1.800));
    }

    #[test]
    fn vpp_ignores_the_mode_bits() {
        // SWC stayed 7-bit at both profiles; an 8-bit decode of 0x78 would
        // report 2.100 V and be dropped as out of range.
        for mode in [0x42, 0x72, 0xFF] {
            let table = BTreeMap::from([(0x2B, mode), (0x21, 0x8C), (0x25, 0x80), (0x27, 0x78)]);
            let rails = decode_rails(&table_reader(table), None).unwrap();
            assert!(close(rails["dram_vpp"], 1.800));
        }
    }

    #[test]
    fn out_of_range_rail_is_dropped() {
        let table = BTreeMap::from([(0x2B, 0x72), (0x21, 0xFF), (0x25, 0x80), (0x27, 0x78)]);
        let rails = decode_rails(&table_reader(table), None).unwrap();
        assert!(!rails.contains_key("dram_vdd"));
        assert!(rails.contains_key("dram_vddq"));
    }

    #[test]
    fn failed_register_read_yields_no_rails() {
        let read = |_register: u8| -> Result<u8, SmbusError> { Err(SmbusError::Bus("bus glitch".into())) };
        assert_eq!(decode_rails(&read, None).unwrap(), PmicRails::new());
    }

    #[test]
    fn the_mode_register_alone_settles_the_vid_encoding() {
        // 0x8C is 1.150 V as 7-bit and 1.500 V as 8-bit. The mode register
        // decides, so no ADC measurement is needed to break the tie.
        let mut table = BTreeMap::from([(0x2B, 0x72), (0x21, 0x8C), (0x25, 0x80), (0x27, 0x78)]);
        let eight_bit = decode_rails(&table_reader(table.clone()), None).unwrap();
        table.insert(0x2B, 0x42);
        let seven_bit = decode_rails(&table_reader(table), None).unwrap();
        assert!(close(eight_bit["dram_vdd"], 1.500));
        assert!(close(seven_bit["dram_vdd"], 1.150));
    }

    // --- DimmTemperatureTest (tests/test_amd_fch_smbus.py): one SPD hub
    // sensor per module, not one number for the pair.

    struct FakeHubs {
        // {(controller, address): celsius}
        temperatures: BTreeMap<(u8, u8), f64>,
        driver_open: bool,
        // What MR0/MR1 answer. Real hubs say 0x51/0x18; a DDR4 SPD EEPROM
        // at the same address answers with ordinary SPD bytes.
        device_type: (u8, u8),
        reads: Mutex<Vec<(u8, u8, u8)>>,
    }

    impl FakeHubs {
        fn new(temperatures: &[((u8, u8), f64)]) -> Arc<Self> {
            Self::build(temperatures, true, None)
        }

        fn build(temperatures: &[((u8, u8), f64)], driver_open: bool, device_type: Option<(u8, u8)>) -> Arc<Self> {
            Arc::new(FakeHubs {
                temperatures: temperatures.iter().copied().collect(),
                driver_open,
                device_type: device_type.unwrap_or(SPD_HUB_DEVICE_TYPE),
                reads: Mutex::new(Vec::new()),
            })
        }
    }

    impl SmbusReader for FakeHubs {
        fn is_driver_open(&self) -> bool {
            self.driver_open
        }

        fn read_byte(&self, address: u8, register: u8, controller: u8) -> Result<u8, SmbusError> {
            self.reads.lock().unwrap().push((controller, address, register));
            let Some(celsius) = self.temperatures.get(&(controller, address)) else {
                return Err(SmbusError::Bus(format!("no device at 0x{address:02X}")));
            };
            let identity = SPD_HUB_DEVICE_TYPE_REGISTER;
            if register == identity {
                return Ok(self.device_type.0);
            }
            if register == identity + 1 {
                return Ok(self.device_type.1);
            }
            let raw = (((celsius / SPD_TEMPERATURE_STEP_C).round_ties_even() as i64) & 0x7FF) << 2;
            let offset = register as i64 - SPD_TEMPERATURE_REGISTER as i64;
            Ok(((raw >> (8 * offset)) & 0xFF) as u8)
        }
    }

    fn read(hubs: &Arc<FakeHubs>) -> DimmTemperatures {
        let hubs = hubs.clone();
        read_dimm_temperatures(Some(&move || Ok(hubs.clone() as Arc<dyn SmbusReader>)), None, None)
    }

    #[test]
    fn hub_address_names_the_channel() {
        assert_eq!(spd_hub_channel(0x50), Some("a"));
        assert_eq!(spd_hub_channel(0x51), Some("a"));
        assert_eq!(spd_hub_channel(0x52), Some("b"));
        assert_eq!(spd_hub_channel(0x53), Some("b"));
        // Beyond the four slots an AM5 board has, claim nothing.
        assert_eq!(spd_hub_channel(0x54), None);
    }

    #[test]
    fn both_populated_hubs_are_read() {
        // The bench layout: 0x51 on channel A, 0x53 on channel B.
        let hubs = FakeHubs::new(&[((0x00, 0x51), 28.75), ((0x00, 0x53), 31.25)]);
        assert_eq!(read(&hubs), BTreeMap::from([("a", 28.75), ("b", 31.25)]));
    }

    #[test]
    fn a_single_dimm_leaves_the_other_channel_absent() {
        let hubs = FakeHubs::new(&[((0x00, 0x51), 30.0)]);
        assert_eq!(read(&hubs), BTreeMap::from([("a", 30.0)]));
    }

    #[test]
    fn the_second_slot_of_a_channel_does_not_overwrite_the_first() {
        let hubs = FakeHubs::new(&[((0x00, 0x50), 25.0), ((0x00, 0x51), 40.0)]);
        assert_eq!(read(&hubs), BTreeMap::from([("a", 25.0)]));
    }

    #[test]
    fn a_device_that_is_not_a_hub_is_not_read_as_one() {
        // A DDR4 module's SPD EEPROM sits at these same addresses and answers
        // a byte read. Its bytes 0x31/0x32 are ordinary SPD content, and here
        // they would decode to 25 C -- inside the band, indistinguishable from
        // a measurement. Only the identity registers tell the two apart.
        let eeprom = FakeHubs::build(&[((0x00, 0x51), 25.0)], true, Some((0x23, 0x13)));
        assert_eq!(read(&eeprom), BTreeMap::new());
    }

    #[test]
    fn a_hub_identifies_itself_before_its_reading_is_used() {
        let hubs = FakeHubs::new(&[((0x00, 0x51), 28.0)]);
        read(&hubs);
        let identity = SPD_HUB_DEVICE_TYPE_REGISTER;
        let asked: Vec<u8> =
            hubs.reads.lock().unwrap().iter().filter(|(_, a, _)| *a == 0x51).map(|(_, _, r)| *r).collect();
        let at = |register| asked.iter().position(|r| *r == register).unwrap();
        assert!(asked.contains(&identity));
        assert!(at(identity) < at(SPD_TEMPERATURE_REGISTER));
    }

    #[test]
    fn an_out_of_range_reading_is_dropped() {
        let hubs = FakeHubs::new(&[((0x00, 0x51), 28.0), ((0x00, 0x53), 200.0)]);
        assert_eq!(read(&hubs), BTreeMap::from([("a", 28.0)]));
    }

    #[test]
    fn no_driver_reports_nothing() {
        let hubs = FakeHubs::build(&[((0x00, 0x51), 28.0)], false, None);
        assert_eq!(read(&hubs), BTreeMap::new());
    }

    #[test]
    fn a_repeat_read_skips_the_bus_scan() {
        let hubs = FakeHubs::new(&[((0x00, 0x51), 28.0), ((0x00, 0x53), 29.0)]);
        read(&hubs);
        let probed_first: std::collections::BTreeSet<(u8, u8)> =
            hubs.reads.lock().unwrap().iter().map(|(c, a, _)| (*c, *a)).collect();
        hubs.reads.lock().unwrap().clear();
        assert_eq!(read(&hubs), BTreeMap::from([("a", 28.0), ("b", 29.0)]));
        let probed_again: std::collections::BTreeSet<(u8, u8)> =
            hubs.reads.lock().unwrap().iter().map(|(c, a, _)| (*c, *a)).collect();
        assert_eq!(probed_again, [(0x00, 0x51), (0x00, 0x53)].into_iter().collect());
        assert!(probed_again.len() < probed_first.len());
    }

    #[test]
    fn the_single_value_helper_reports_the_warmer_module() {
        let hubs = FakeHubs::new(&[((0x00, 0x51), 28.0), ((0x00, 0x53), 33.0)]);
        assert_eq!(read_dimm_temperature(Some(&move || Ok(hubs.clone() as Arc<dyn SmbusReader>))), Some(33.0));
    }

    #[test]
    fn the_single_value_helper_reports_none_when_no_hub_answers() {
        let hubs = FakeHubs::new(&[]);
        assert_eq!(read_dimm_temperature(Some(&move || Ok(hubs.clone() as Arc<dyn SmbusReader>))), None);
    }
}
