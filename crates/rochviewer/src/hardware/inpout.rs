//! The inpoutx64 driver, loaded at runtime.
//!
//! Loaded with `LoadLibrary` on first use rather than linked, because it is a
//! third-party component the user supplies: a machine without it must still
//! get a window that says so.

use std::ffi::c_void;
use std::sync::OnceLock;

use libloading::Library;

use super::driver_path::{find_driver, missing_message};
use super::{Hardware, HwError, HwResult};

type MapPhysToLin = unsafe extern "system" fn(*mut u8, u32, *mut *mut c_void) -> *mut u8;
type UnmapPhysicalMemory = unsafe extern "system" fn(*mut c_void, *mut u8) -> i32;
type GetPhysLong = unsafe extern "system" fn(*mut u8, *mut u32) -> i32;
type SetPhysLong = unsafe extern "system" fn(*mut u8, u32) -> i32;
type ReadPortUchar = unsafe extern "system" fn(u16) -> u8;
type WritePortUchar = unsafe extern "system" fn(u16, u8);
type ReadPortUlong = unsafe extern "system" fn(u32) -> u32;
type WritePortUlong = unsafe extern "system" fn(u32, u32);
type IsInpOutDriverOpen = unsafe extern "system" fn() -> u32;

pub struct InpOut {
    _library: Library,
    map_phys_to_lin: MapPhysToLin,
    unmap_physical_memory: UnmapPhysicalMemory,
    get_phys_long: GetPhysLong,
    set_phys_long: SetPhysLong,
    read_port_uchar: ReadPortUchar,
    write_port_uchar: WritePortUchar,
    read_port_ulong: ReadPortUlong,
    write_port_ulong: WritePortUlong,
    is_open: IsInpOutDriverOpen,
}

static LOAD_ERROR: OnceLock<Option<String>> = OnceLock::new();

impl InpOut {
    /// Load the DLL and resolve every export. Called once per process.
    pub fn load() -> HwResult<Self> {
        let result = Self::load_uncached();
        let _ = LOAD_ERROR.set(result.as_ref().err().map(|e| e.0.clone()));
        result
    }

    /// The reason the last load failed, if it did. Loads once if nothing
    /// has tried yet.
    pub fn load_error() -> Option<String> {
        if LOAD_ERROR.get().is_none() {
            // hw() is what performs the load; asking it settles the answer.
            let _ = super::hw();
        }
        LOAD_ERROR.get().cloned().flatten()
    }

    fn load_uncached() -> HwResult<Self> {
        let path = find_driver().ok_or_else(|| HwError(missing_message()))?;
        // SAFETY: loading a DLL runs its DllMain. This is the driver the
        // user placed beside the program for exactly this purpose.
        let library = unsafe { Library::new(&path) }.map_err(|e| {
            HwError(format!(
                "Found {} but could not load it. It may be the wrong architecture, \
                 or the process may not be running as administrator ({e}).",
                path.display()
            ))
        })?;
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {{
                // SAFETY: the signature matches inpoutx64's documented export.
                let symbol = unsafe { library.get::<$ty>($name) }.map_err(|e| {
                    HwError(format!(
                        "{} does not export {}: {e}",
                        path.display(),
                        String::from_utf8_lossy(&$name[..$name.len() - 1])
                    ))
                })?;
                *symbol
            }};
        }
        let map_phys_to_lin = symbol!(b"MapPhysToLin\0", MapPhysToLin);
        let unmap_physical_memory = symbol!(b"UnmapPhysicalMemory\0", UnmapPhysicalMemory);
        let get_phys_long = symbol!(b"GetPhysLong\0", GetPhysLong);
        let set_phys_long = symbol!(b"SetPhysLong\0", SetPhysLong);
        let read_port_uchar = symbol!(b"DlPortReadPortUchar\0", ReadPortUchar);
        let write_port_uchar = symbol!(b"DlPortWritePortUchar\0", WritePortUchar);
        let read_port_ulong = symbol!(b"DlPortReadPortUlong\0", ReadPortUlong);
        let write_port_ulong = symbol!(b"DlPortWritePortUlong\0", WritePortUlong);
        let is_open = symbol!(b"IsInpOutDriverOpen\0", IsInpOutDriverOpen);
        Ok(InpOut {
            _library: library,
            map_phys_to_lin,
            unmap_physical_memory,
            get_phys_long,
            set_phys_long,
            read_port_uchar,
            write_port_uchar,
            read_port_ulong,
            write_port_ulong,
            is_open,
        })
    }
}

impl Hardware for InpOut {
    fn read_phys(&self, address: u64, size: usize) -> HwResult<Vec<u8>> {
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut handle: *mut c_void = std::ptr::null_mut();
        // SAFETY: MapPhysToLin maps `size` bytes of physical memory and
        // returns a user-mode pointer to them, or null on failure.
        let virt = unsafe {
            (self.map_phys_to_lin)(address as *mut u8, size as u32, &mut handle)
        };
        if virt.is_null() {
            return Err(HwError(format!(
                "Failed to map physical address 0x{address:016X}. \
                 Ensure the driver is running with administrator privileges."
            )));
        }
        let mut out = Vec::with_capacity(size);
        // SAFETY: `virt` is valid for `size` bytes until unmapped below.
        // Registers are read as aligned dwords where the size allows: MMIO
        // decodes the access width, and a byte read of a dword register is
        // not guaranteed to return the same bits.
        unsafe {
            if size % 4 == 0 && (virt as usize) % 4 == 0 {
                for i in 0..size / 4 {
                    let dword = std::ptr::read_volatile((virt as *const u32).add(i));
                    out.extend_from_slice(&dword.to_le_bytes());
                }
            } else {
                for i in 0..size {
                    out.push(std::ptr::read_volatile(virt.add(i)));
                }
            }
            if (self.unmap_physical_memory)(handle, virt) == 0 {
                eprintln!("Warning: Failed to unmap physical memory at 0x{:016X}", virt as usize);
            }
        }
        Ok(out)
    }

    fn get_phys_long(&self, address: u64) -> HwResult<u32> {
        let mut value = 0u32;
        // SAFETY: GetPhysLong writes one DWORD through the out pointer.
        let ok = unsafe { (self.get_phys_long)(address as *mut u8, &mut value) };
        if ok == 0 {
            return Err(HwError(format!("GetPhysLong failed at 0x{address:016X}")));
        }
        Ok(value)
    }

    fn set_phys_long(&self, address: u64, value: u32) -> HwResult<()> {
        // SAFETY: writes one DWORD of physical memory; callers restrict this
        // to the SMU mailbox registers.
        let ok = unsafe { (self.set_phys_long)(address as *mut u8, value) };
        if ok == 0 {
            return Err(HwError(format!("SetPhysLong failed at 0x{address:016X}")));
        }
        Ok(())
    }

    fn inb(&self, port: u16) -> HwResult<u8> {
        // SAFETY: port read through the driver.
        Ok(unsafe { (self.read_port_uchar)(port) })
    }

    fn outb(&self, port: u16, value: u8) -> HwResult<()> {
        // SAFETY: port write through the driver.
        unsafe { (self.write_port_uchar)(port, value) };
        Ok(())
    }

    fn inl(&self, port: u16) -> HwResult<u32> {
        // SAFETY: port read through the driver.
        Ok(unsafe { (self.read_port_ulong)(port as u32) })
    }

    fn outl(&self, port: u16, value: u32) -> HwResult<()> {
        // SAFETY: port write through the driver.
        unsafe { (self.write_port_ulong)(port as u32, value) };
        Ok(())
    }

    fn is_driver_open(&self) -> bool {
        // SAFETY: no arguments; returns whether the kernel driver is open.
        unsafe { (self.is_open)() != 0 }
    }
}
