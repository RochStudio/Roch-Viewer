//! The privileged primitives on Linux, and what they refuse.
//!
//! This runs with `CAP_SYS_RAWIO` -- as root, or in the helper -- so the
//! project's rule that there is no general write API is enforced here rather
//! than by convention:
//!
//! * `/dev/mem` is opened read-only. A physical write has nowhere to go: the
//!   only ones the readers issue are the AMD SMN selector and data window, and
//!   those are PCI configuration registers, which are served through sysfs.
//! * RAM cannot be read. Kernels built with `CONFIG_STRICT_DEVMEM` refuse it
//!   anyway; this refuses it on the ones that are not, since nothing here has any
//!   business in it and the helper may be runnable without a password.
//! * A PCI configuration write can reach exactly those two registers, on the
//!   host bridge, and nothing else.
//! * The legacy ports below 0x200 are refused except the Super I/O and EC index
//!   pairs, and 0xCF8-0xCFF always are. 0xCF9 is the reset control register: one
//!   byte there restarts the machine.
//! * An SMBus transfer goes through the same allowlists the raw transports use.
//! * Only firmware tables can be read as files.
//!
//! Three things differ from reading through a Windows driver, each because the
//! kernel is using the same hardware:
//!
//! * **Configuration space goes through sysfs, not ECAM or CF8/CFC.** The kernel
//!   uses CF8/CFC itself for the first 256 bytes of bus 0, under a lock user
//!   space cannot take, so moving the selector under it could send one of its
//!   own writes to the wrong register. ECAM is usually refused anyway: the kernel
//!   claims it busy and `CONFIG_IO_STRICT_DEVMEM` blocks busy regions. The callers
//!   keep asking in ECAM addresses, and this translates them.
//! * **A card powered down to D3cold reads as absent.** Touching its configuration
//!   space would wake it, and on some machines a card that fails to come back
//!   blocks everything that touches it, unkillably.
//! * **SMBus transfers go through `/dev/i2c-*`** where the kernel has an adapter,
//!   so the adapter's own lock serialises them. See `intel_pch_smbus`.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::sync::Mutex;

use super::super::pci_mcfg::{McfgAllocation, parse_mcfg};
use super::super::{Hardware, HwError, HwResult, KernelSmbus};
use crate::hardware::lowlevel_io::{CF8_PORT, CFC_PORT, PCI_CONFIG_ENABLE, SMN_DATA_REG, SMN_INDEX_REG};
use crate::intel::intel_pch_smbus::{
    DDR4_SPD_PAGE_SELECT_ADDRESSES, DDR5_ADDRESSES, SPD_HUB_ADDRESSES, SPD_HUB_PAGE_MASK, SPD_HUB_PAGE_REGISTER,
    THERMAL_SENSOR_ADDRESSES, WRITE_ALLOWLIST,
};

const PAGE_SIZE: u64 = 4096;
const IOMEM_PATH: &str = "/proc/iomem";
const MCFG_PATH: &str = "/sys/firmware/acpi/tables/MCFG";
const LOCKDOWN_PATH: &str = "/sys/kernel/security/lockdown";
/// EC RAM as the `ec_sys` module publishes it. Every byte read through this goes
/// through the kernel's EC driver and its lock; the ports themselves are that
/// driver's, and interleaving a raw transaction with it can wedge the controller.
const EC_IO_PATH: &str = "/sys/kernel/debug/ec/ec0/io";

/// The most a single physical read may ask for.
pub const MAX_PHYS_READ: usize = 1 << 22;

/// Files the helper will read, as prefixes. Firmware tables only.
pub const FIRMWARE_FILE_PREFIXES: [&str; 3] =
    ["/sys/firmware/acpi/tables/", "/sys/firmware/dmi/tables/", "/sys/class/dmi/id/"];

// linux/i2c-dev.h.
const I2C_SLAVE_FORCE: libc::c_ulong = 0x0706;
const I2C_SMBUS: libc::c_ulong = 0x0720;
// linux/i2c.h.
const I2C_SMBUS_READ: u8 = 1;
const I2C_SMBUS_WRITE: u8 = 0;
const I2C_SMBUS_BYTE: u32 = 1;
const I2C_SMBUS_BYTE_DATA: u32 = 2;
const I2C_SMBUS_WORD_DATA: u32 = 3;

/// `union i2c_smbus_data`: a byte, a word, or a 34-byte block.
#[repr(C)]
union I2cSmbusData {
    byte: u8,
    word: u16,
    block: [u8; 34],
}

/// `struct i2c_smbus_ioctl_data`.
#[repr(C)]
struct I2cSmbusIoctlData {
    read_write: u8,
    command: u8,
    size: u32,
    data: *mut I2cSmbusData,
}

/// The physical ranges the kernel calls System RAM, from `/proc/iomem`.
///
/// A process without the capability to see them gets zeros there instead, which
/// parse as an empty range and refuse nothing; but such a process could not open
/// `/dev/mem` either.
pub fn system_ram_ranges(iomem: &str) -> Vec<(u64, u64)> {
    iomem
        .lines()
        .filter_map(|line| {
            let (range, name) = line.split_once(" : ")?;
            if name.trim() != "System RAM" {
                return None;
            }
            let (first, last) = range.trim().split_once('-')?;
            let (first, last) = (u64::from_str_radix(first, 16).ok()?, u64::from_str_radix(last, 16).ok()?);
            (last > first).then_some((first, last))
        })
        .collect()
}

/// Whether `size` bytes at `address` touch any of `ranges`.
pub fn overlaps(ranges: &[(u64, u64)], address: u64, size: u64) -> bool {
    let end = address.saturating_add(size.saturating_sub(1));
    ranges.iter().any(|(first, last)| address <= *last && end >= *first)
}

/// `/dev/i2c-*` only exists with the `i2c-dev` module loaded, which most
/// distributions leave to whoever needs it, so as root this asks for it. It lasts
/// until the next boot and changes nothing on disk. The helper `rochviewer setup`
/// installs has capabilities rather than root and cannot, so that setup arranges
/// it. Without it the SMBus falls back to the raw transport.
fn load_i2c_support() {
    if !super::is_root() || std::path::Path::new("/sys/class/i2c-dev").exists() {
        return;
    }
    let _ = std::process::Command::new("modprobe")
        .args(["-q", "i2c-dev"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Whether a port may be touched at all.
///
/// 0xCF8-0xCFF are served without touching the ports (see the module docs), and
/// nothing below 0x200 is needed but the Super I/O index pairs and the EC. What
/// lives down there includes the keyboard controller's reset line at 0x64 and
/// fast reset at 0x92; 0xCF9 is the reset control register.
pub fn port_is_allowed(port: u16) -> bool {
    if (0xCF8..=0xCFF).contains(&port) {
        return false;
    }
    port >= 0x200 || matches!(port, 0x2E | 0x2F | 0x4E | 0x4F | 0x62 | 0x66)
}

/// Whether an SMBus transfer is one the raw transports would also make.
pub fn smbus_is_allowed(address: u8, transfer: KernelSmbus) -> bool {
    match transfer {
        KernelSmbus::ReadByte { .. } => DDR5_ADDRESSES.contains(&address),
        KernelSmbus::ReadWord { .. } => THERMAL_SENSOR_ADDRESSES.contains(&address),
        // The PMIC's ADC selector, and an SPD hub's page selector and only as a
        // page number: never its EEPROM window, where a stray write would corrupt a
        // module's SPD. AMD selects pages this way; Intel has SPD Write Disable to
        // get past, which is why it does not, and the armed bit refuses this anyway.
        KernelSmbus::WriteByte { register, value } => {
            WRITE_ALLOWLIST.contains(&(address, register))
                || (SPD_HUB_ADDRESSES.contains(&address)
                    && register == SPD_HUB_PAGE_REGISTER
                    && value & !SPD_HUB_PAGE_MASK == 0)
        }
        KernelSmbus::SendByte { value } => value == 0 && DDR4_SPD_PAGE_SELECT_ADDRESSES.contains(&address),
    }
}

/// Whether a path is a firmware table this will read for a client.
pub fn firmware_file_is_allowed(path: &str) -> bool {
    !path.split('/').any(|part| part == "..")
        && FIRMWARE_FILE_PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

/// Whether a PCI configuration write is one of the two the SMN reads need.
pub fn pci_write_is_allowed(location: PciFunction, register: u32) -> bool {
    location == (PciFunction { segment: 0, bus: 0, device: 0, function: 0 })
        && (register == SMN_INDEX_REG || register == SMN_DATA_REG)
}

/// One PCI function's place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PciFunction {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciFunction {
    fn sysfs(&self, leaf: &str) -> String {
        format!(
            "/sys/bus/pci/devices/{:04x}:{:02x}:{:02x}.{}/{leaf}",
            self.segment, self.bus, self.device, self.function
        )
    }
}

/// Where in configuration space an ECAM address falls, or `None`.
pub fn ecam_location(allocations: &[McfgAllocation], address: u64) -> Option<(PciFunction, u32)> {
    let entry = allocations.iter().find(|entry| {
        let buses = entry.end_bus as u64 - entry.start_bus as u64 + 1;
        address >= entry.base_address && address < entry.base_address + (buses << 20)
    })?;
    let offset = address - entry.base_address;
    Some((
        PciFunction {
            segment: entry.segment,
            bus: (entry.start_bus as u64 + (offset >> 20)) as u8,
            device: ((offset >> 15) & 0x1F) as u8,
            function: ((offset >> 12) & 0x7) as u8,
        },
        (offset & 0xFFF) as u32,
    ))
}

/// What a CF8 value selects, or `None` with the enable bit clear.
pub fn cf8_location(cf8: u32) -> Option<(PciFunction, u32)> {
    if cf8 & PCI_CONFIG_ENABLE == 0 {
        return None;
    }
    Some((
        PciFunction {
            segment: 0,
            bus: ((cf8 >> 16) & 0xFF) as u8,
            device: ((cf8 >> 11) & 0x1F) as u8,
            function: ((cf8 >> 8) & 0x7) as u8,
        },
        cf8 & 0xFC,
    ))
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

thread_local! {
    /// `iopl` is per thread, and a thread that has not asked faults on its
    /// first `in`. Threads started before the first port access never inherit it.
    static IOPL_RAISED: Cell<bool> = const { Cell::new(false) };
}

fn raise_iopl() -> Result<(), std::io::Error> {
    // SAFETY: a plain syscall that changes nothing but this thread's I/O
    // privilege level, and which fails cleanly without CAP_SYS_RAWIO.
    if unsafe { libc::syscall(libc::SYS_iopl, 3 as libc::c_long) } == 0 { Ok(()) } else { Err(last_error()) }
}

fn ensure_iopl() -> HwResult<()> {
    if IOPL_RAISED.with(Cell::get) {
        return Ok(());
    }
    raise_iopl().map_err(|error| HwError(format!("Could not get port access: {error}")))?;
    IOPL_RAISED.with(|raised| raised.set(true));
    Ok(())
}

#[cfg(target_arch = "x86_64")]
mod port {
    use std::arch::asm;

    // SAFETY for all four: the caller has raised the I/O privilege level and
    // checked the port. None of these touches memory.
    pub unsafe fn inb(port: u16) -> u8 {
        let value: u8;
        unsafe { asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags)) };
        value
    }
    pub unsafe fn outb(port: u16, value: u8) {
        unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)) };
    }
    pub unsafe fn inl(port: u16) -> u32 {
        let value: u32;
        unsafe { asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags)) };
        value
    }
    pub unsafe fn outl(port: u16, value: u32) {
        unsafe { asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags)) };
    }
}

/// The real thing: `/dev/mem`, the ports, sysfs and `/dev/i2c-*`.
pub struct Direct {
    memory: File,
    /// Physical page to the address it is mapped at. Kept for the process: a
    /// reader revisits the same few pages a thousand times a second, and a
    /// map/unmap pair per read is what made this slow on the driver.
    pages: Mutex<HashMap<u64, usize>>,
    ecam: Vec<McfgAllocation>,
    system_ram: Vec<(u64, u64)>,
    configuration: Mutex<HashMap<PciFunction, Option<File>>>,
    i2c: Mutex<HashMap<u32, File>>,
    i2c_transfer: Mutex<()>,
    /// The CF8 selector a caller believes it wrote. Kept here so the real port,
    /// which the kernel is using, is never moved.
    cf8: Mutex<u32>,
}

impl Direct {
    /// Open everything, or say why not. Fails quickly and quietly without
    /// access, which is how an ordinary process finds out it needs the helper.
    pub fn open() -> Result<Direct, String> {
        let memory = std::fs::OpenOptions::new()
            .read(true)
            // O_SYNC is what asks for an uncached mapping. Registers must not
            // be read through the cache.
            .custom_flags(libc::O_SYNC)
            .open("/dev/mem")
            .map_err(|error| access_refused("/dev/mem", &error))?;
        raise_iopl().map_err(|error| access_refused("the I/O ports", &error))?;
        IOPL_RAISED.with(|raised| raised.set(true));
        // Not fatal. Without it every configuration read answers as absent,
        // and the reason is that the machine has no ECAM to translate.
        let ecam = std::fs::read(MCFG_PATH).ok().and_then(|table| parse_mcfg(&table).ok()).unwrap_or_default();
        let system_ram = std::fs::read_to_string(IOMEM_PATH).map(|text| system_ram_ranges(&text)).unwrap_or_default();
        load_i2c_support();
        Ok(Direct {
            memory,
            pages: Mutex::new(HashMap::new()),
            ecam,
            system_ram,
            configuration: Mutex::new(HashMap::new()),
            i2c: Mutex::new(HashMap::new()),
            i2c_transfer: Mutex::new(()),
            cf8: Mutex::new(0),
        })
    }

    fn page(&self, physical: u64) -> HwResult<usize> {
        let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mapped) = pages.get(&physical) {
            return Ok(*mapped);
        }
        // SAFETY: a fresh read-only shared mapping of one page, at an address
        // the kernel chooses. The pointer is only ever read through, volatile.
        let mapped = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE_SIZE as usize,
                libc::PROT_READ,
                libc::MAP_SHARED,
                self.memory.as_raw_fd(),
                physical as libc::off_t,
            )
        };
        if mapped == libc::MAP_FAILED {
            let error = last_error();
            return Err(HwError(format!("Failed to map physical address 0x{physical:016X}: {error}")));
        }
        pages.insert(physical, mapped as usize);
        Ok(mapped as usize)
    }

    /// `size` bytes of MMIO, as aligned dwords where the size allows: MMIO
    /// decodes the access width, and a byte read of a dword register is not
    /// guaranteed to return the same bits.
    fn read_mmio(&self, address: u64, size: usize) -> HwResult<Vec<u8>> {
        if overlaps(&self.system_ram, address, size as u64) {
            return Err(HwError(format!("Refused a read of system memory at 0x{address:016X}")));
        }
        let mut out = Vec::with_capacity(size);
        let as_dwords = size.is_multiple_of(4) && address.is_multiple_of(4);
        let mut at = address;
        let end = address + size as u64;
        while at < end {
            let page_start = at & !(PAGE_SIZE - 1);
            let within = (at - page_start) as usize;
            let page_end = (page_start + PAGE_SIZE).min(end);
            let base = self.page(page_start)?;
            // SAFETY: `base` is a live one-page mapping that is never unmapped,
            // and `within` plus the count stays inside it. The loads are
            // volatile because these are registers.
            unsafe {
                let start = (base + within) as *const u8;
                let count = (page_end - at) as usize;
                if as_dwords {
                    for index in 0..count / 4 {
                        out.extend_from_slice(&std::ptr::read_volatile((start as *const u32).add(index)).to_le_bytes());
                    }
                } else {
                    for index in 0..count {
                        out.push(std::ptr::read_volatile(start.add(index)));
                    }
                }
            }
            at = page_end;
        }
        Ok(out)
    }

    /// One configuration dword. All ones for a function that is not there, or
    /// one asleep in D3cold, as a bus with nothing answering reads.
    fn read_configuration(&self, location: PciFunction, register: u32) -> HwResult<u32> {
        if std::fs::read_to_string(location.sysfs("power_state")).is_ok_and(|state| state.trim() == "D3cold") {
            return Ok(u32::MAX);
        }
        let mut files = self.configuration.lock().unwrap_or_else(|e| e.into_inner());
        let file = files.entry(location).or_insert_with(|| File::open(location.sysfs("config")).ok());
        let Some(file) = file else { return Ok(u32::MAX) };
        let aligned = register & !3;
        let mut bytes = [0xFFu8; 4];
        // Short past the end: a conventional function has 256 bytes, and only
        // CAP_SYS_ADMIN is shown more than the first 64.
        let count = file
            .read_at(&mut bytes, aligned as u64)
            .map_err(|error| HwError(format!("Could not read the configuration of {location:?}: {error}")))?;
        if count < 4 {
            return Ok(u32::MAX);
        }
        Ok(u32::from_le_bytes(bytes))
    }

    fn write_configuration(&self, location: PciFunction, register: u32, value: u32) -> HwResult<()> {
        if !pci_write_is_allowed(location, register) {
            return Err(HwError(format!("Refused a configuration write to {location:?} register 0x{register:02X}")));
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(location.sysfs("config"))
            .map_err(|error| HwError(format!("Could not open the host bridge's configuration: {error}")))?;
        file.write_all_at(&value.to_le_bytes(), register as u64)
            .map_err(|error| HwError(format!("Could not write the SMN selector: {error}")))
    }

    /// The adapter's device, opened the first time it is asked for.
    fn i2c_device(&self, adapter: u32) -> HwResult<i32> {
        let mut devices = self.i2c.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(file) = devices.get(&adapter) {
            return Ok(file.as_raw_fd());
        }
        let path = format!("/dev/i2c-{adapter}");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| HwError(format!("Could not open {path}: {error}")))?;
        let descriptor = file.as_raw_fd();
        devices.insert(adapter, file);
        Ok(descriptor)
    }
}

/// What to say when the access itself is refused. Lockdown gets its own answer,
/// because no amount of privilege gets past it.
fn access_refused(what: &str, error: &std::io::Error) -> String {
    let lockdown = std::fs::read_to_string(LOCKDOWN_PATH).unwrap_or_default();
    if lockdown_is_active(&lockdown) {
        return format!(
            "The kernel is in lockdown ({}), which blocks {what} for every process, root included. \
             Secure Boot usually turns it on.",
            lockdown.trim()
        );
    }
    format!("Could not open {what}: {error}")
}

/// `[none] integrity confidentiality`: the active mode is the bracketed one.
pub fn lockdown_is_active(text: &str) -> bool {
    !text.trim().is_empty() && !text.contains("[none]")
}

impl Hardware for Direct {
    fn read_phys(&self, address: u64, size: usize) -> HwResult<Vec<u8>> {
        if size > MAX_PHYS_READ {
            return Err(HwError(format!("Refused a physical read of {size} bytes")));
        }
        if size == 0 {
            return Ok(Vec::new());
        }
        if ecam_location(&self.ecam, address).is_none() {
            return self.read_mmio(address, size);
        }
        // Configuration space, a dword at a time, however it was asked for.
        let mut out = Vec::with_capacity(size + 4);
        let mut at = address & !3;
        while (out.len() as u64) < (address & 3) + size as u64 {
            let (location, register) = ecam_location(&self.ecam, at)
                .ok_or_else(|| HwError(format!("0x{at:016X} is outside configuration space")))?;
            out.extend_from_slice(&self.read_configuration(location, register)?.to_le_bytes());
            at += 4;
        }
        let skip = (address & 3) as usize;
        Ok(out[skip..skip + size].to_vec())
    }

    fn get_phys_long(&self, address: u64) -> HwResult<u32> {
        match ecam_location(&self.ecam, address) {
            Some((location, register)) => self.read_configuration(location, register),
            None => {
                let bytes = self.read_mmio(address, 4)?;
                Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }
        }
    }

    fn set_phys_long(&self, address: u64, value: u32) -> HwResult<()> {
        match ecam_location(&self.ecam, address) {
            Some((location, register)) => self.write_configuration(location, register, value),
            None => Err(HwError(format!("Refused a physical write to 0x{address:016X}"))),
        }
    }

    fn inb(&self, port: u16) -> HwResult<u8> {
        check_port(port)?;
        ensure_iopl()?;
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: the privilege level is raised and the port is checked.
            Ok(unsafe { port::inb(port) })
        }
        #[cfg(not(target_arch = "x86_64"))]
        Err(HwError("There are no I/O ports on this architecture".into()))
    }

    fn outb(&self, port: u16, value: u8) -> HwResult<()> {
        check_port(port)?;
        ensure_iopl()?;
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: the privilege level is raised and the port is checked.
            unsafe { port::outb(port, value) };
            Ok(())
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = value;
            Err(HwError("There are no I/O ports on this architecture".into()))
        }
    }

    fn inl(&self, port: u16) -> HwResult<u32> {
        if port == CF8_PORT {
            return Ok(*self.cf8.lock().unwrap_or_else(|e| e.into_inner()));
        }
        if port == CFC_PORT {
            let selected = *self.cf8.lock().unwrap_or_else(|e| e.into_inner());
            return match cf8_location(selected) {
                Some((location, register)) => self.read_configuration(location, register),
                None => Ok(u32::MAX),
            };
        }
        check_port(port)?;
        ensure_iopl()?;
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: the privilege level is raised and the port is checked.
            Ok(unsafe { port::inl(port) })
        }
        #[cfg(not(target_arch = "x86_64"))]
        Err(HwError("There are no I/O ports on this architecture".into()))
    }

    fn outl(&self, port: u16, value: u32) -> HwResult<()> {
        if port == CF8_PORT {
            *self.cf8.lock().unwrap_or_else(|e| e.into_inner()) = value;
            return Ok(());
        }
        if port == CFC_PORT {
            let selected = *self.cf8.lock().unwrap_or_else(|e| e.into_inner());
            let Some((location, register)) = cf8_location(selected) else { return Ok(()) };
            return self.write_configuration(location, register, value);
        }
        check_port(port)?;
        ensure_iopl()?;
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: the privilege level is raised and the port is checked.
            unsafe { port::outl(port, value) };
            Ok(())
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = value;
            Err(HwError("There are no I/O ports on this architecture".into()))
        }
    }

    fn is_driver_open(&self) -> bool {
        true
    }

    fn read_firmware_file(&self, path: &str) -> HwResult<Vec<u8>> {
        if !firmware_file_is_allowed(path) {
            return Err(HwError(format!("Refused to read {path}")));
        }
        std::fs::read(path).map_err(|error| HwError(format!("Could not read {path}: {error}")))
    }

    fn kernel_smbus(&self, adapter: u32, address: u8, transfer: KernelSmbus) -> HwResult<u16> {
        if !smbus_is_allowed(address, transfer) {
            return Err(HwError(format!("Refused the SMBus transfer {transfer:?} at 0x{address:02X}")));
        }
        // Held across the transfer: the slave address is state on the descriptor,
        // and another thread must not change it between these two calls.
        let device = self.i2c_device(adapter)?;
        let _transfer = self.i2c_transfer.lock().unwrap_or_else(|e| e.into_inner());
        // FORCE, because the kernel's spd5118 driver holds the SPD hubs. Going
        // through the adapter is still what serialises this against it.
        // SAFETY: an fd of ours and an integer argument.
        if unsafe { libc::ioctl(device, I2C_SLAVE_FORCE, address as libc::c_ulong) } < 0 {
            return Err(HwError(format!("Could not address 0x{address:02X}: {}", last_error())));
        }
        let mut data = I2cSmbusData { block: [0; 34] };
        let (read_write, command, size) = match transfer {
            KernelSmbus::ReadByte { register } => (I2C_SMBUS_READ, register, I2C_SMBUS_BYTE_DATA),
            KernelSmbus::ReadWord { register } => (I2C_SMBUS_READ, register, I2C_SMBUS_WORD_DATA),
            KernelSmbus::WriteByte { register, value } => {
                data.byte = value;
                (I2C_SMBUS_WRITE, register, I2C_SMBUS_BYTE_DATA)
            }
            // Send Byte carries its one byte in the command.
            KernelSmbus::SendByte { value } => (I2C_SMBUS_WRITE, value, I2C_SMBUS_BYTE),
        };
        let mut request = I2cSmbusIoctlData { read_write, command, size, data: &mut data };
        // SAFETY: `request` and the union it points at are live locals of the
        // layouts the ioctl documents.
        if unsafe { libc::ioctl(device, I2C_SMBUS, &mut request as *mut I2cSmbusIoctlData) } < 0 {
            return Err(HwError(format!("SMBus transfer at 0x{address:02X} failed: {}", last_error())));
        }
        // SAFETY: every bit pattern of the union is valid for a u16, and the
        // block was zeroed first.
        Ok(match transfer {
            KernelSmbus::ReadByte { .. } => unsafe { data.byte as u16 },
            KernelSmbus::ReadWord { .. } => unsafe { data.word },
            _ => 0,
        })
    }

    fn kernel_ec_byte(&self, register: u8) -> HwResult<u8> {
        if !std::path::Path::new(EC_IO_PATH).exists() && super::is_root() {
            // Lasts until the next boot and is read-only unless asked otherwise.
            let _ = std::process::Command::new("modprobe")
                .args(["-q", "ec_sys"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        let file = File::open(EC_IO_PATH).map_err(|error| HwError(format!("Could not open {EC_IO_PATH}: {error}")))?;
        let mut byte = [0u8];
        match file.read_at(&mut byte, register as u64) {
            Ok(1) => Ok(byte[0]),
            Ok(_) => Err(HwError(format!("{EC_IO_PATH} has no register 0x{register:02X}"))),
            Err(error) => Err(HwError(format!("Could not read EC register 0x{register:02X}: {error}"))),
        }
    }
}

fn check_port(port: u16) -> HwResult<()> {
    if port_is_allowed(port) { Ok(()) } else { Err(HwError(format!("Refused access to port 0x{port:04X}"))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALLOCATION: McfgAllocation =
        McfgAllocation { base_address: 0xC000_0000, segment: 0, start_bus: 0, end_bus: 255 };

    const IOMEM: &str = "\
00000000-00000fff : Reserved
00001000-0009efff : System RAM
000a0000-000fffff : Reserved
00100000-5fffffff : System RAM
  1a000000-1b7fffff : Kernel code
c0000000-cfffffff : PCI ECAM 0000 [bus 00-ff]
fedc0000-fedcffff : pnp 00:04
100000000-107fffffff : System RAM
";

    #[test]
    fn system_memory_is_found_and_refused() {
        let ranges = system_ram_ranges(IOMEM);
        assert_eq!(ranges, vec![(0x1000, 0x9EFFF), (0x10_0000, 0x5FFF_FFFF), (0x1_0000_0000, 0x10_7FFF_FFFF)]);
        assert!(overlaps(&ranges, 0x10_0000, 4), "the first byte");
        assert!(overlaps(&ranges, 0x5FFF_FFFF, 1), "the last byte");
        assert!(overlaps(&ranges, 0xFFFF_FFFC, 8), "a read running into it");
        assert!(!overlaps(&ranges, 0xFEDC_0000, 4), "MCHBAR");
        assert!(!overlaps(&ranges, 0x6000_0000, 4), "just past it");
    }

    #[test]
    fn an_iomem_this_process_may_not_see_refuses_nothing() {
        // Without the capability every address reads as zero, and the process
        // could not open /dev/mem to use the answer anyway.
        assert_eq!(system_ram_ranges("00000000-00000000 : System RAM\n"), vec![]);
    }

    #[test]
    fn the_reset_ports_can_never_be_reached() {
        for port in [0xCF9u16, 0x64, 0x92, 0x60, 0x70, 0x20, 0xA0] {
            assert!(!port_is_allowed(port), "{port:#x}");
        }
    }

    #[test]
    fn the_ports_the_readers_use_are_reachable() {
        for port in [0x2Eu16, 0x2F, 0x4E, 0x4F, 0x62, 0x66, 0x290, 0x295, 0xA45, 0xCD6, 0xCD7, 0xB00, 0x5000, 0xEFA0] {
            assert!(port_is_allowed(port), "{port:#x}");
        }
    }

    #[test]
    fn the_config_ports_are_never_touched_for_real() {
        for port in 0xCF8u16..=0xCFF {
            assert!(!port_is_allowed(port), "{port:#x}");
        }
    }

    #[test]
    fn an_ecam_address_comes_apart_into_its_function_and_register() {
        // The Z890's SMBus function, as the search reads it.
        let (location, register) = ecam_location(&[ALLOCATION], 0xC000_0000 + (0x80 << 20) + (0x1F << 15) + (4 << 12) + 0x20).unwrap();
        assert_eq!(location, PciFunction { segment: 0, bus: 0x80, device: 0x1F, function: 4 });
        assert_eq!(register, 0x20);
        assert_eq!(location.sysfs("config"), "/sys/bus/pci/devices/0000:80:1f.4/config");
    }

    #[test]
    fn mmio_is_not_mistaken_for_configuration_space() {
        // MCHBAR, above the ECAM window here.
        assert_eq!(ecam_location(&[ALLOCATION], 0xFEDC_0000), None);
        assert_eq!(ecam_location(&[ALLOCATION], 0xBFFF_FFFF), None);
        assert!(ecam_location(&[ALLOCATION], 0xCFFF_FFFF).is_some());
        assert_eq!(ecam_location(&[ALLOCATION], 0xD000_0000), None);
    }

    #[test]
    fn a_partial_allocation_ends_where_its_buses_do() {
        let entry = McfgAllocation { base_address: 0xE000_0000, segment: 1, start_bus: 0x10, end_bus: 0x1F };
        let (location, _) = ecam_location(&[entry], 0xE000_0000).unwrap();
        assert_eq!((location.segment, location.bus), (1, 0x10));
        assert_eq!(ecam_location(&[entry], 0xE000_0000 + (0x10 << 20)), None);
    }

    #[test]
    fn cf8_decodes_as_the_port_would() {
        let (location, register) = cf8_location(0x8000_FC20).unwrap();
        assert_eq!(location, PciFunction { segment: 0, bus: 0, device: 0x1F, function: 4 });
        assert_eq!(register, 0x20);
        assert_eq!(cf8_location(0x0000_FC20), None);
    }

    #[test]
    fn a_config_write_reaches_only_the_smn_window() {
        let host = PciFunction { segment: 0, bus: 0, device: 0, function: 0 };
        assert!(pci_write_is_allowed(host, 0x60));
        assert!(pci_write_is_allowed(host, 0x64));
        assert!(!pci_write_is_allowed(host, 0x04), "the command register");
        assert!(!pci_write_is_allowed(host, 0x68));
        assert!(!pci_write_is_allowed(PciFunction { function: 1, ..host }, 0x60));
        assert!(!pci_write_is_allowed(PciFunction { bus: 1, ..host }, 0x60));
    }

    #[test]
    fn the_smbus_allowlists_hold_on_this_path_too() {
        // The one PMIC selector, and no rail.
        assert!(smbus_is_allowed(0x48, KernelSmbus::WriteByte { register: 0x30, value: 5 }));
        for register in [0x21, 0x25, 0x27, 0x2B, 0x31] {
            assert!(!smbus_is_allowed(0x48, KernelSmbus::WriteByte { register, value: 5 }), "{register:#x}");
        }
        // Nothing can be written to an SPD hub through here but its page number:
        // not its EEPROM window, and not a value that is not a page.
        assert!(smbus_is_allowed(0x50, KernelSmbus::WriteByte { register: 0x0B, value: 7 }));
        assert!(!smbus_is_allowed(0x50, KernelSmbus::WriteByte { register: 0x0B, value: 8 }));
        for register in [0x00, 0x0A, 0x0C, 0x80, 0xFF] {
            assert!(!smbus_is_allowed(0x50, KernelSmbus::WriteByte { register, value: 0 }), "{register:#x}");
        }
        assert!(smbus_is_allowed(0x50, KernelSmbus::ReadByte { register: 0x80 }));
        assert!(!smbus_is_allowed(0x77, KernelSmbus::ReadByte { register: 0 }));
        assert!(smbus_is_allowed(0x18, KernelSmbus::ReadWord { register: 5 }));
        assert!(!smbus_is_allowed(0x50, KernelSmbus::ReadWord { register: 5 }));
        // Only DDR4's two fixed page selectors, only as zero.
        assert!(smbus_is_allowed(0x36, KernelSmbus::SendByte { value: 0 }));
        assert!(!smbus_is_allowed(0x36, KernelSmbus::SendByte { value: 1 }));
        assert!(!smbus_is_allowed(0x50, KernelSmbus::SendByte { value: 0 }));
    }

    #[test]
    fn only_firmware_tables_can_be_read_as_files() {
        assert!(firmware_file_is_allowed("/sys/firmware/acpi/tables/MCFG"));
        assert!(firmware_file_is_allowed("/sys/firmware/dmi/tables/DMI"));
        assert!(firmware_file_is_allowed("/sys/class/dmi/id/board_serial"));
        assert!(!firmware_file_is_allowed("/etc/shadow"));
        assert!(!firmware_file_is_allowed("/sys/firmware/dmi/tables/../../../../etc/shadow"));
    }

    #[test]
    fn lockdown_reads_from_the_bracketed_mode() {
        assert!(!lockdown_is_active("[none] integrity confidentiality\n"));
        assert!(lockdown_is_active("none [integrity] confidentiality\n"));
        assert!(!lockdown_is_active(""));
    }
}
