//! Test fixtures shared across modules.
//!
//! [`FakeHardware`] stands in for the driver on the calling test thread:
//! physical memory is a byte map, ports answer from a handler or from the
//! last value written, and every access is logged so a test can assert on
//! which registers were touched -- which is what most of the Python tests
//! asserted. WMI fakes are [`crate::wmi_util::FakeWmi`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::hardware::{Hardware, HwError, HwResult, set_thread_hardware};

/// One logged access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    ReadPhys { address: u64, size: usize },
    GetPhysLong(u64),
    SetPhysLong(u64, u32),
    InB(u16),
    OutB(u16, u8),
    InL(u16),
    OutL(u16, u32),
}

pub type PortReader = dyn Fn(&FakeHardware, u16, usize) -> Option<u32> + Send + Sync;

/// A scriptable stand-in for the driver.
pub struct FakeHardware {
    memory: Mutex<HashMap<u64, u8>>,
    ports: Mutex<HashMap<u16, u32>>,
    port_reader: Option<Box<PortReader>>,
    log: Mutex<Vec<Access>>,
    pub driver_open: bool,
    /// When set, unmapped physical reads return 0xFF bytes instead of failing.
    pub unmapped_reads_ones: bool,
}

impl Default for FakeHardware {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeHardware {
    pub fn new() -> Self {
        FakeHardware {
            memory: Mutex::new(HashMap::new()),
            ports: Mutex::new(HashMap::new()),
            port_reader: None,
            log: Mutex::new(Vec::new()),
            driver_open: true,
            unmapped_reads_ones: false,
        }
    }

    pub fn with_dword(self, address: u64, value: u32) -> Self {
        self.with_bytes(address, &value.to_le_bytes())
    }

    pub fn with_qword(self, address: u64, value: u64) -> Self {
        self.with_bytes(address, &value.to_le_bytes())
    }

    pub fn with_bytes(self, address: u64, bytes: &[u8]) -> Self {
        {
            let mut memory = self.memory.lock().unwrap();
            for (i, b) in bytes.iter().enumerate() {
                memory.insert(address + i as u64, *b);
            }
        }
        self
    }

    pub fn with_port(self, port: u16, value: u32) -> Self {
        self.ports.lock().unwrap().insert(port, value);
        self
    }

    /// Answer port reads from `reader(fake, port, width_bytes)`; `None` falls
    /// back to the last value written to that port.
    pub fn with_port_reader(
        mut self,
        reader: impl Fn(&FakeHardware, u16, usize) -> Option<u32> + Send + Sync + 'static,
    ) -> Self {
        self.port_reader = Some(Box::new(reader));
        self
    }

    pub fn driver_closed(mut self) -> Self {
        self.driver_open = false;
        self
    }

    /// Install for the calling thread; the returned handle reads the log.
    pub fn install(self) -> Arc<FakeHardware> {
        let shared = Arc::new(self);
        set_thread_hardware(Some(shared.clone()));
        shared
    }

    pub fn log(&self) -> Vec<Access> {
        self.log.lock().unwrap().clone()
    }

    pub fn clear_log(&self) {
        self.log.lock().unwrap().clear();
    }

    /// The last value written to a port, if any.
    pub fn port_value(&self, port: u16) -> Option<u32> {
        self.ports.lock().unwrap().get(&port).copied()
    }

    pub fn set_dword(&self, address: u64, value: u32) {
        let mut memory = self.memory.lock().unwrap();
        for (i, b) in value.to_le_bytes().iter().enumerate() {
            memory.insert(address + i as u64, *b);
        }
    }

    /// Physical addresses read, in order.
    pub fn phys_reads(&self) -> Vec<u64> {
        self.log()
            .into_iter()
            .filter_map(|a| match a {
                Access::ReadPhys { address, .. } | Access::GetPhysLong(address) => Some(address),
                _ => None,
            })
            .collect()
    }

    fn record(&self, access: Access) {
        self.log.lock().unwrap().push(access);
    }

    fn read_port(&self, port: u16, width: usize) -> u32 {
        if let Some(reader) = &self.port_reader {
            if let Some(value) = reader(self, port, width) {
                return value;
            }
        }
        self.port_value(port).unwrap_or(if width == 1 { 0xFF } else { 0xFFFF_FFFF })
    }
}

impl Hardware for FakeHardware {
    fn read_phys(&self, address: u64, size: usize) -> HwResult<Vec<u8>> {
        self.record(Access::ReadPhys { address, size });
        let memory = self.memory.lock().unwrap();
        let mut out = Vec::with_capacity(size);
        for i in 0..size as u64 {
            match memory.get(&(address + i)) {
                Some(b) => out.push(*b),
                None if self.unmapped_reads_ones => out.push(0xFF),
                None => return Err(HwError(format!("unmapped 0x{address:X}"))),
            }
        }
        Ok(out)
    }

    fn get_phys_long(&self, address: u64) -> HwResult<u32> {
        self.record(Access::GetPhysLong(address));
        let memory = self.memory.lock().unwrap();
        let mut bytes = [0u8; 4];
        for (i, slot) in bytes.iter_mut().enumerate() {
            *slot = match memory.get(&(address + i as u64)) {
                Some(b) => *b,
                None if self.unmapped_reads_ones => 0xFF,
                None => return Err(HwError(format!("GetPhysLong failed at 0x{address:016X}"))),
            };
        }
        Ok(u32::from_le_bytes(bytes))
    }

    fn set_phys_long(&self, address: u64, value: u32) -> HwResult<()> {
        self.record(Access::SetPhysLong(address, value));
        self.set_dword(address, value);
        Ok(())
    }

    fn inb(&self, port: u16) -> HwResult<u8> {
        self.record(Access::InB(port));
        Ok(self.read_port(port, 1) as u8)
    }

    fn outb(&self, port: u16, value: u8) -> HwResult<()> {
        self.record(Access::OutB(port, value));
        self.ports.lock().unwrap().insert(port, value as u32);
        Ok(())
    }

    fn inl(&self, port: u16) -> HwResult<u32> {
        self.record(Access::InL(port));
        Ok(self.read_port(port, 4))
    }

    fn outl(&self, port: u16, value: u32) -> HwResult<()> {
        self.record(Access::OutL(port, value));
        self.ports.lock().unwrap().insert(port, value);
        Ok(())
    }

    fn is_driver_open(&self) -> bool {
        self.driver_open
    }
}
