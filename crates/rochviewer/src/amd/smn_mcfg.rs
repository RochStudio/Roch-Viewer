//! AM5 SMN reader through ACPI MCFG / PCIe ECAM.
//!
//! The public surface is read-only. Each SMN read requires one PCI
//! configuration write to the AMD SMN index register (0x60), followed by a
//! read from 0x64.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::hardware::pci_mcfg::{McfgAllocation, ecam_address, get_mcfg_table, parse_mcfg, select_allocation};
use crate::hardware::{HwError, HwResult, hw};

use super::smn::{
    AMD_VENDOR_ID, DEFAULT_MUTEX_NAME, NamedPciMutex, PciMutex, SMN_DATA_REG, SMN_INDEX_REG, SmnTransport,
    VENDOR_REG, all_none,
};

/// PCI 00:00.0 config dwords mapped through the ACPI MCFG table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InpOutMcfgConfigIO {
    pub allocation: McfgAllocation,
    pub config_base: u64,
    // Close over the one permitted physical destination. No method in this
    // transport accepts a caller-provided address or config offset to write.
    selector_address: u64,
}

impl InpOutMcfgConfigIO {
    /// The firmware's own MCFG table, segment 0, bus 0.
    ///
    /// Fails when the driver DLL is missing (as the Python constructor's
    /// `InpOutPortIO()` did) or the MCFG table cannot be read or used.
    pub fn new() -> Result<Self, String> {
        if let Some(error) = crate::hardware::driver_error() {
            return Err(error);
        }
        Self::from_table(&get_mcfg_table()?, 0, 0)
    }

    /// Map config space through an MCFG table the caller already has.
    pub fn from_table(table: &[u8], segment: u16, bus: u8) -> Result<Self, String> {
        let entries = parse_mcfg(table)?;
        let allocation = select_allocation(&entries, segment, bus)?;
        let config_base = ecam_address(&allocation, bus, 0, 0, 0)?;
        Ok(InpOutMcfgConfigIO { allocation, config_base, selector_address: config_base + SMN_INDEX_REG as u64 })
    }

    pub fn is_driver_open(&self) -> bool {
        hw().is_driver_open()
    }

    pub fn read_dword(&self, register: u32) -> HwResult<u32> {
        if register & 3 != 0 || register > 0xFFC {
            return Err(HwError::new("PCI config dword register is invalid"));
        }
        hw().get_phys_long(self.config_base + register as u64)
    }

    /// Write only PCI config offset 0x60, the required SMN selector.
    pub fn write_selector(&self, value: u32) -> HwResult<()> {
        let selector_address = self.selector_address;
        hw().set_phys_long(selector_address, value)
            .map_err(|_| HwError(format!("SetPhysLong failed at selector 0x{selector_address:016X}")))
    }
}

/// Serialized, AMD-gated, read-only SMN transport over PCIe ECAM.
pub struct McfgSmnReader {
    config: InpOutMcfgConfigIO,
    mutex: Box<dyn PciMutex>,
    lock: Mutex<()>,
    last_error: Mutex<String>,
}

impl McfgSmnReader {
    /// The firmware's ECAM window on the shared `Global\Access_PCI` mutex.
    pub fn new() -> Result<Self, String> {
        Ok(Self::with_config(InpOutMcfgConfigIO::new()?, Box::new(NamedPciMutex::new(DEFAULT_MUTEX_NAME))))
    }

    pub fn with_config(config: InpOutMcfgConfigIO, mutex: Box<dyn PciMutex>) -> Self {
        McfgSmnReader { config, mutex, lock: Mutex::new(()), last_error: Mutex::new(String::new()) }
    }

    pub fn config(&self) -> &InpOutMcfgConfigIO {
        &self.config
    }

    fn set_last_error(&self, message: String) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = message;
    }

    /// The serialized part of a batch read: `Ok(true)` when every value was
    /// read, `Ok(false)` when the vendor gate refused.
    fn transaction(&self, addresses: &[u32], results: &mut BTreeMap<u32, Option<u32>>) -> HwResult<bool> {
        let _lock = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _mutex = self.mutex.acquire()?;
        let vendor = self.config.read_dword(VENDOR_REG)? & 0xFFFF;
        if vendor != AMD_VENDOR_ID {
            self.set_last_error(format!("MCFG PCI 00:00.0 vendor is 0x{vendor:04X}, expected AMD 0x1022"));
            return Ok(false);
        }

        let prior_selector = self.config.read_dword(SMN_INDEX_REG)?;
        let mut selector_attempted = false;
        let mut body = || -> HwResult<()> {
            for address in addresses {
                // Mark before the call: a driver can report failure after
                // partially completing the physical write.
                selector_attempted = true;
                self.config.write_selector(*address)?;
                let value = self.config.read_dword(SMN_DATA_REG)?;
                if value == 0xFFFF_FFFF {
                    return Err(HwError(format!("SMN 0x{address:08X} returned invalid 0xFFFFFFFF")));
                }
                results.insert(*address, Some(value));
            }
            Ok(())
        };
        let mut result = body();
        if selector_attempted {
            // A failing restore replaces whatever the body raised, as a raise
            // inside Python's `finally` does.
            if let Err(error) = self.config.write_selector(prior_selector) {
                result = Err(error);
            }
        }
        result.map(|()| true)
    }
}

impl SmnTransport for McfgSmnReader {
    fn read_many(&self, smn_addresses: &[u32]) -> BTreeMap<u32, Option<u32>> {
        let addresses = smn_addresses.to_vec();
        let mut results = all_none(&addresses);
        self.set_last_error(String::new());
        if !self.config.is_driver_open() {
            self.set_last_error("InpOut driver is not open".into());
            return results;
        }
        match self.transaction(&addresses, &mut results) {
            Ok(true) => results,
            Ok(false) => all_none(&addresses),
            Err(error) => {
                self.set_last_error(format!("MCFG SMN read failed: {error}"));
                all_none(&addresses)
            }
        }
    }

    fn last_error(&self) -> String {
        self.last_error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amd::smn::tests::FakeMutex;
    use crate::hardware::pci_mcfg::sample_table;
    use crate::hardware::{Hardware, set_thread_hardware};
    use std::sync::Arc;

    const BASE: u64 = 0xE000_0000;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Call {
        IsOpen,
        Read(u32),
        WriteSelector(u32),
    }

    struct State {
        selector: u32,
        calls: Vec<Call>,
        failed: bool,
    }

    /// The ECAM view of 00:00.0: the config dwords at BASE + register, with
    /// the SMN window behind 0x60/0x64.
    struct FakeConfigIo {
        vendor: u32,
        smn: BTreeMap<u32, u32>,
        open: bool,
        state: Mutex<State>,
        /// Fail the first selector write of this value, after making it.
        fail_write_of: Option<u32>,
        /// Refuse to restore the prior selector.
        fail_restore: bool,
    }

    impl FakeConfigIo {
        fn new(smn: &[(u32, u32)]) -> Self {
            FakeConfigIo {
                vendor: 0x1022,
                smn: smn.iter().copied().collect(),
                open: true,
                state: Mutex::new(State { selector: 0xABCD_EF00, calls: Vec::new(), failed: false }),
                fail_write_of: None,
                fail_restore: false,
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.state.lock().unwrap().calls.clone()
        }

        fn selector(&self) -> u32 {
            self.state.lock().unwrap().selector
        }

        fn selector_writes(&self) -> Vec<Call> {
            self.calls().into_iter().filter(|call| matches!(call, Call::WriteSelector(_))).collect()
        }
    }

    impl Hardware for FakeConfigIo {
        fn read_phys(&self, _address: u64, _size: usize) -> HwResult<Vec<u8>> {
            panic!("unexpected mapped read")
        }
        fn get_phys_long(&self, address: u64) -> HwResult<u32> {
            let register = (address - BASE) as u32;
            let mut state = self.state.lock().unwrap();
            state.calls.push(Call::Read(register));
            match register {
                0 => Ok(0x1234_0000 | self.vendor),
                0x60 => Ok(state.selector),
                0x64 => Ok(self.smn.get(&state.selector).copied().unwrap_or(0)),
                other => panic!("unexpected register 0x{other:X}"),
            }
        }
        fn set_phys_long(&self, address: u64, value: u32) -> HwResult<()> {
            assert_eq!(address, BASE + 0x60, "only the selector may be written");
            let mut state = self.state.lock().unwrap();
            state.calls.push(Call::WriteSelector(value));
            if self.fail_restore && value == 0xABCD_EF00 && state.selector != value {
                return Err(HwError::new("restore failed"));
            }
            state.selector = value;
            if !state.failed && self.fail_write_of == Some(value) {
                state.failed = true;
                return Err(HwError::new("reported write failure"));
            }
            Ok(())
        }
        fn inb(&self, _port: u16) -> HwResult<u8> {
            panic!("unexpected port access")
        }
        fn outb(&self, _port: u16, _value: u8) -> HwResult<()> {
            panic!("unexpected port access")
        }
        fn inl(&self, _port: u16) -> HwResult<u32> {
            panic!("unexpected port access")
        }
        fn outl(&self, _port: u16, _value: u32) -> HwResult<()> {
            panic!("unexpected port access")
        }
        fn is_driver_open(&self) -> bool {
            self.state.lock().unwrap().calls.push(Call::IsOpen);
            self.open
        }
    }

    fn install(fake: FakeConfigIo) -> Arc<FakeConfigIo> {
        let shared = Arc::new(fake);
        set_thread_hardware(Some(shared.clone()));
        shared
    }

    fn mcfg_table() -> Vec<u8> {
        sample_table(&[(BASE, 0, 0, 0xFF)])
    }

    fn config() -> InpOutMcfgConfigIO {
        InpOutMcfgConfigIO::from_table(&mcfg_table(), 0, 0).unwrap()
    }

    fn reader_with(mutex: &FakeMutex) -> McfgSmnReader {
        McfgSmnReader::with_config(config(), Box::new(mutex.clone()))
    }

    fn reader() -> McfgSmnReader {
        reader_with(&FakeMutex::default())
    }

    #[test]
    fn reads_batch_and_restores_prior_selector() {
        let io = install(FakeConfigIo::new(&[(0x50204, 0x1111), (0x50208, 0x2222)]));
        let mutex = FakeMutex::default();
        let reader = reader_with(&mutex);
        assert_eq!(
            reader.read_many(&[0x50204, 0x50208]),
            BTreeMap::from([(0x50204, Some(0x1111)), (0x50208, Some(0x2222))])
        );
        assert_eq!(io.selector(), 0xABCD_EF00);
        assert_eq!(mutex.counts(), (1, 1));
        assert_eq!(
            io.selector_writes(),
            vec![Call::WriteSelector(0x50204), Call::WriteSelector(0x50208), Call::WriteSelector(0xABCD_EF00)]
        );
    }

    #[test]
    fn foreign_vendor_prevents_selector_write() {
        let mut fake = FakeConfigIo::new(&[]);
        fake.vendor = 0x8086;
        let io = install(fake);
        let reader = reader();
        assert_eq!(reader.read(0x50204), None);
        assert!(reader.last_error().to_lowercase().contains("vendor"));
        assert!(io.selector_writes().is_empty());
    }

    #[test]
    fn closed_driver_prevents_config_access() {
        let mut fake = FakeConfigIo::new(&[]);
        fake.open = false;
        let io = install(fake);
        assert_eq!(reader().read(0x50204), None);
        assert_eq!(io.calls(), vec![Call::IsOpen]);
    }

    #[test]
    fn has_no_public_smn_write_api() {
        // Python checked McfgSmnReader has no `write` / `smn_write`
        // attribute; nothing in the shipped part of this file defines one.
        let source = include_str!("smn_mcfg.rs");
        let shipped = source.split("#[cfg(test)]").next().unwrap();
        assert!(!shipped.contains("fn write("));
        assert!(!shipped.contains("fn smn_write("));
    }

    #[test]
    fn all_ones_invalidates_the_complete_batch() {
        let io = install(FakeConfigIo::new(&[(0x50204, 0x1111), (0x50208, 0xFFFF_FFFF)]));
        let reader = reader();
        assert_eq!(reader.read_many(&[0x50204, 0x50208]), BTreeMap::from([(0x50204, None), (0x50208, None)]));
        assert!(reader.last_error().to_lowercase().contains("0xffffffff"));
        assert_eq!(io.selector(), 0xABCD_EF00);
    }

    #[test]
    fn failed_selector_write_still_attempts_restore() {
        let mut fake = FakeConfigIo::new(&[]);
        fake.fail_write_of = Some(0x50204);
        let io = install(fake);
        assert_eq!(reader().read(0x50204), None);
        assert_eq!(io.selector(), 0xABCD_EF00);
        assert_eq!(io.selector_writes(), vec![Call::WriteSelector(0x50204), Call::WriteSelector(0xABCD_EF00)]);
    }

    #[test]
    fn real_config_transport_exposes_selector_write_only() {
        // Python: InpOutMcfgConfigIO has write_selector and no write_dword,
        // and the physical access has no _write_dword.
        let source = include_str!("smn_mcfg.rs");
        let shipped = source.split("#[cfg(test)]").next().unwrap();
        assert!(shipped.contains("pub fn write_selector("));
        assert!(!shipped.contains("fn write_dword("));
        assert_eq!(shipped.matches("set_phys_long(").count(), 1);
    }

    #[test]
    fn real_config_transport_hardcodes_ecam_selector_destination() {
        let fake = crate::testing::FakeHardware::new().install();
        let config = config();
        config.write_selector(0x50204).unwrap();
        assert_eq!(fake.log(), vec![crate::testing::Access::SetPhysLong(0xE000_0060, 0x50204)]);
    }

    #[test]
    fn config_dword_reads_require_aligned_registers() {
        // Python also refused -4; a u32 register cannot be negative.
        let config = config();
        for register in [1u32, 0x1000, 0xFFFF_FFFC] {
            assert_eq!(config.read_dword(register).unwrap_err().0, "PCI config dword register is invalid");
        }
    }

    #[test]
    fn restore_failure_rejects_complete_snapshot() {
        let mut fake = FakeConfigIo::new(&[(0x50204, 0x1111)]);
        fake.fail_restore = true;
        install(fake);
        let reader = reader();
        assert_eq!(reader.read(0x50204), None);
        // The Python faked write_selector itself and saw its own "restore
        // failed". Faked one level down, the driver's refusal arrives worded
        // the way the real transport words a failed SetPhysLong.
        assert_eq!(
            reader.last_error(),
            "MCFG SMN read failed: SetPhysLong failed at selector 0x00000000E0000060"
        );
    }
}
