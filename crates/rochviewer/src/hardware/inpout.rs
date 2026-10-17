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
        //
        // With the flags ctypes.WinDLL passes for a full path, so the DLL's own
        // dependencies are searched for where the Python searched for them.
        let library = unsafe {
            libloading::os::windows::Library::load_with_flags(
                &path,
                libloading::os::windows::LOAD_LIBRARY_SEARCH_DEFAULT_DIRS
                    | libloading::os::windows::LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
            )
        }
        .map(Library::from)
        .map_err(|e| {
            let reason = match &e {
                // WindowsError's Debug is the bare code GetLastError gave.
                libloading::Error::LoadLibraryExW { source } => match format!("{source:?}").parse::<u32>() {
                    Ok(code) => ctypes_load_error(code, &path.display().to_string()),
                    Err(_) => e.to_string(),
                },
                _ => e.to_string(),
            };
            HwError(format!(
                "Found {} but could not load it. It may be the wrong architecture, \
                 or the process may not be running as administrator ({reason}).",
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

/// The text `str(exc)` gave for the OSError ctypes raised when LoadLibraryExW
/// failed with `code`: CPython's own wording, which the driver notice quotes.
///
/// A missing module (126) is ctypes' FileNotFoundError naming the path; any
/// other code is `PyErr_SetFromWindowsErr`'s "[WinError n] message", the
/// system message with its trailing whitespace and full stops removed.
fn ctypes_load_error(code: u32, path: &str) -> String {
    const ERROR_MOD_NOT_FOUND: u32 = 126;
    if code == ERROR_MOD_NOT_FOUND {
        return format!(
            "Could not find module '{path}' (or one of its dependencies). \
             Try using the full path with constructor syntax."
        );
    }
    format!("[WinError {code}] {}", windows_error_message(code, system_message(code)))
}

/// CPython's tidy-up of a FormatMessageW result, or its fallback when there
/// was none.
fn windows_error_message(code: u32, message: Option<String>) -> String {
    match message {
        Some(text) => text.trim_end_matches(|c: char| c <= ' ' || c == '.').to_string(),
        None => format!("Windows Error 0x{code:x}"),
    }
}

/// FormatMessageW's text for a system error code, in the language CPython
/// asks for (neutral, default sublanguage), or `None`.
fn system_message(code: u32) -> Option<String> {
    use windows::Win32::System::Diagnostics::Debug::{
        FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW,
    };
    let mut buffer = [0u16; 2048];
    // SAFETY: the buffer is ours and its length is passed; no inserts.
    let length = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            None,
            code,
            0x0400,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            buffer.len() as u32,
            None,
        )
    } as usize;
    (length != 0).then(|| String::from_utf16_lossy(&buffer[..length.min(buffer.len())]))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_load_is_described_in_cpythons_words() {
        // Python 1.1.0's notice quoted str(OSError) from ctypes.WinDLL:
        // "[WinError 193] %1 is not a valid Win32 application" for a file
        // that is not a DLL, where this said only "LoadLibraryExW failed".
        assert_eq!(
            windows_error_message(193, Some("%1 is not a valid Win32 application.\r\n".into())),
            "%1 is not a valid Win32 application"
        );
        assert_eq!(windows_error_message(5, None), "Windows Error 0x5");
        assert_eq!(
            ctypes_load_error(126, r"C:\x\inpoutx64.dll"),
            r"Could not find module 'C:\x\inpoutx64.dll' (or one of its dependencies). Try using the full path with constructor syntax."
        );
        assert!(ctypes_load_error(193, "x").starts_with("[WinError 193] "));
    }
}
