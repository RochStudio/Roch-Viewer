// Roch Viewer -- a read-only memory-controller and timing viewer.
// Copyright (C) 2026 Roch Studio
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! What the graphics card reports about itself.
//!
//! CPU-Z's Graphics tab is part reading and part database. This module ships the
//! reading half, from three sources that need no driver of our own: the adapter's
//! PNP device id for what the card is, and NVAPI and NVML for what it holds.
//!
//! The NVAPI entry points are not in NVIDIA's published header -- they are
//! resolved by ID through nvapi_QueryInterface, the same way GPU-Z and HWiNFO
//! reach them -- so every value was checked against CPU-Z's report for this card
//! before being trusted:
//!
//! ```text
//!     board GIGABYTE (subsystem 0x1458), revision A1, AD104-A, 5888 cores,
//!     12282 MB of GDDR6X by Micron on a 192-bit bus.
//! ```
//!
//! All of them agreed, and the two a second interface also answers (cores, via
//! NVML; bus width, via GetRamBusWidth) agreed with themselves as well.
//!
//! Two values on that tab cannot be read at all: ROP and TM unit counts, which
//! have no entry point on this driver, and a chip's SKU, which has none anywhere.
//! Neither is shown. The code name is the one the driver reports -- the core
//! family, "AD104" -- rather than a SKU a table would have to claim.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{c_char, c_void};
use std::sync::{Arc, LazyLock, Mutex};

use libloading::os::windows::{
    LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library,
};
use regex::Regex;

use crate::memo::Memo;
use crate::pyfmt;
use crate::system_identity::{AdapterIdentity, parse_pnp_identity, py_strip};
use crate::wmi_util;

pub const NVIDIA_VENDOR_ID: u16 = 0x10DE;

/// PCI subsystem vendors, i.e. who assembled the board. 0x1458 is measured on
/// this bench, where CPU-Z names it GIGABYTE Technology; the rest are the other
/// major add-in-board vendors. Unlisted vendors print their ID rather than a
/// guess, as elsewhere in this project.
pub const BOARD_VENDORS: &[(u16, &str)] = &[
    (0x1043, "ASUSTeK Computer"),
    (0x1458, "GIGABYTE Technology"),
    (0x1462, "MSI"),
    (0x1569, "Palit Microsystems"),
    (0x196E, "PNY"),
    (0x19DA, "Zotac"),
    (0x1B4C, "KFA2"),
    (0x3842, "EVGA"),
    (0x7377, "Colorful"),
    (0x10DE, "NVIDIA"),
];

/// NVML architecture -> process node, keyed on the architecture because that is
/// what the node is a property of. 8 is Ada, measured here against CPU-Z's 4 nm;
/// the others are the neighbouring client architectures. A datacentre part of
/// the same generation can be built on a different node, so an unlisted
/// architecture reports nothing.
pub const NVML_ARCHITECTURE_NODES: &[(u32, &str)] = &[
    (6, "12 nm"),  // Turing
    (7, "8 nm"),   // Ampere, GA10x client
    (8, "4 nm"),   // Ada -- verified against CPU-Z on this bench
    (10, "4 nm"),  // Blackwell, GB20x client
];

/// NVAPI's memory type and maker enumerations, neither of which is published.
/// Only values a bench has returned are named, each beside what CPU-Z or GPU-Z
/// read from the same card: 15 and 10 with GDDR6X by Micron, and 16 and 6 on
/// an RTX 5070 Ti (GB203) whose GPU-Z line reads "GDDR7 (Hynix)". The first
/// measurement contradicted the ordering these enumerations are usually quoted
/// with, which is why nothing here is filled in from memory -- an unmeasured
/// code prints as itself.
pub const NVAPI_RAM_TYPES: &[(u32, &str)] = &[(15, "GDDR6X"), (16, "GDDR7")];
pub const NVAPI_RAM_MAKERS: &[(u32, &str)] = &[(6, "SK hynix"), (10, "Micron")];

pub const NVAPI_QUERIES: &[(&str, u32)] = &[
    ("initialize", 0x0150E828),
    ("enum_gpus", 0xE5AC921F),
    ("core_count", 0xC7026A87),
    ("short_name", 0xD988F0F3),
    ("ram_type", 0x57F7CAAC),
    ("ram_maker", 0x42AEA16A),
    ("ram_bus_width", 0x7975C581),
    ("frame_buffer_kb", 0x46FBEB03),
    // Core voltage. Undocumented, like the four above it, and confirmed the
    // same way: it returns 960000 uV where HWiNFO reads 0.960 V on this card.
    ("core_voltage", 0x465F9BCF),
];

// The entry point shapes. The Python built each CFUNCTYPE once rather than per
// call, because a CFUNCTYPE is a type and rebuilding it on every read cost more
// than the call it wrapped; here they are types to begin with. NVAPI is
// __cdecl, which on x64 is the one calling convention there is.
type QueryInterfaceFn = unsafe extern "C" fn(u32) -> *mut c_void;
type StatusFn = unsafe extern "C" fn() -> i32;
type EnumFn = unsafe extern "C" fn(*mut *mut c_void, *mut u32) -> i32;
type UintOutFn = unsafe extern "C" fn(*mut c_void, *mut u32) -> i32;
type TextOutFn = unsafe extern "C" fn(*mut c_void, *mut c_char) -> i32;
type StructOutFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;

/// The core-voltage status block: the value, with padding either side.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NvapiVoltStatus {
    version: u32,
    flags: u32,
    unknown1: [u32; 8],
    value_uv: u32,
    unknown2: [u32; 8],
}

// 76 bytes, as ctypes sized it; the version word below is built from this.
const _: () = assert!(std::mem::size_of::<NvapiVoltStatus>() == 76);

/// "PCI\VEN_10DE&DEV_2786&SUBSYS_40C61458&REV_A1\4&256A0AA8&0&0008"
static PNP_IDENTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)VEN_([0-9A-F]{4})&DEV_([0-9A-F]{4})&SUBSYS_([0-9A-F]{4})([0-9A-F]{4})&REV_([0-9A-F]{2})",
    )
    .expect("the PNP identity pattern is valid")
});

/// What the card reports, keyed by field ("code_name", "memory_size", ...),
/// as display strings. Missing keys mean the card did not answer.
pub type GpuInfo = BTreeMap<&'static str, String>;

/// Live readings keyed by row name ("GPU Clock", "GPU Temperature", ...).
pub type GpuSensors = BTreeMap<&'static str, String>;

static CACHE: Memo<GpuInfo> = Memo::new();

/// Load a vendor library the way ctypes does on Python 3.8 and later: the
/// application directory, System32 and the added DLL directories for a bare
/// name -- not the current directory or PATH -- and a full path as given.
///
/// Kept loaded for the life of the process once it has loaded, as ctypes
/// keeps every library it loads: NVAPI is initialised and never unloaded,
/// and freeing the library under it would pull its code out from under the
/// session the voltage row keeps. A failed load is tried again next time,
/// as constructing the ctypes library again did.
fn load_library(name: &'static str) -> Result<&'static Library, String> {
    static LOADED: Mutex<Vec<(&'static str, &'static Library)>> = Mutex::new(Vec::new());
    let mut loaded = LOADED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((_, library)) = loaded.iter().find(|(known, _)| *known == name) {
        return Ok(library);
    }
    let mut flags = LOAD_LIBRARY_SEARCH_DEFAULT_DIRS;
    if name.contains('\\') || name.contains('/') {
        flags |= LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR;
    }
    // SAFETY: loading a vendor library runs its initialisation routines,
    // which is what loading it in the Python did too; nothing else is
    // assumed about it here.
    let library = unsafe { Library::load_with_flags(name, flags) }.map_err(|error| error.to_string())?;
    let library: &'static Library = Box::leak(Box::new(library));
    loaded.push((name, library));
    Ok(library)
}

/// One export of `library` as the function type `T`, or `None`.
fn symbol<T: Copy>(library: &Library, name: &str) -> Option<T> {
    // SAFETY: every caller names `T` as the export's documented signature, a
    // plain function pointer the size of FARPROC (libloading checks the size).
    unsafe { library.get::<T>(name) }.ok().map(|found| *found)
}

/// A NUL-terminated buffer as Python's `buffer.value.decode("ascii",
/// "ignore").strip()`.
fn c_text(buffer: &[u8]) -> String {
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    let text: String = buffer[..end].iter().filter(|b| b.is_ascii()).map(|b| *b as char).collect();
    py_strip(&text).to_string()
}

/// Ask WMI for the display adapters, when the caller has not already.
fn own_pnp_device_ids() -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    for adapter in wmi_util::query("Win32_VideoController")? {
        let name = adapter.str_or_empty("Name");
        let name = py_strip(&name);
        if !name.is_empty() && !name.contains("Basic Display") {
            found.push(adapter.str_or_empty("PNPDeviceID"));
        }
    }
    Ok(found)
}

/// The display adapter's PCI identity, or `None`.
///
/// Read from the PNP device id Windows already holds, which carries all four
/// fields: vendor, device, subsystem and revision. Walking PCI configuration
/// space for the same four cost a map/unmap pair per dword across 64
/// functions, and returned the first VGA-class device it found -- the
/// integrated graphics, on a machine that has both.
///
/// The ids are taken from the caller where it has them. Opening a WMI
/// connection of our own costs about 90 ms, and the one caller in this
/// project has already queried and cached that class for the card's name.
fn adapter_identity(pnp_device_ids: Option<&[String]>) -> Option<AdapterIdentity> {
    let own;
    let ids = match pnp_device_ids {
        Some(ids) => ids,
        None => {
            own = own_pnp_device_ids().ok()?;
            &own
        }
    };
    ids.iter().find_map(|id| parse_pnp_identity(&PNP_IDENTITY, id))
}

#[cfg(test)]
thread_local! {
    /// The nvapi_QueryInterface a test stands in for the real one.
    static TEST_NVAPI: std::cell::Cell<Option<QueryInterfaceFn>> = const { std::cell::Cell::new(None) };
    /// What `nvml_query` answers on a test thread.
    static TEST_NVML: std::cell::RefCell<Option<Box<dyn Fn() -> Result<NvmlFacts, String>>>> =
        const { std::cell::RefCell::new(None) };
}

/// The handful of NVAPI entry points this module uses.
struct Nvapi {
    /// Entry point addresses by name, for the ones the driver resolved.
    resolved: HashMap<&'static str, usize>,
    /// The first GPU's handle.
    gpu: usize,
}

impl Nvapi {
    fn open() -> Result<Nvapi, String> {
        #[cfg(test)]
        {
            match TEST_NVAPI.with(|slot| slot.get()) {
                Some(query) => Nvapi::resolve(query),
                None => Err("NVAPI is not loaded in unit tests".into()),
            }
        }
        #[cfg(not(test))]
        {
            let library = load_library("nvapi64.dll")?;
            let query: QueryInterfaceFn = symbol(library, "nvapi_QueryInterface")
                .ok_or_else(|| "nvapi64.dll has no nvapi_QueryInterface".to_string())?;
            Nvapi::resolve(query)
        }
    }

    fn resolve(query: QueryInterfaceFn) -> Result<Nvapi, String> {
        let mut resolved = HashMap::new();
        for (name, identifier) in NVAPI_QUERIES {
            // SAFETY: nvapi_QueryInterface takes an interface id and returns
            // an address or null; it has no other requirement.
            let pointer = unsafe { query(*identifier) };
            if !pointer.is_null() {
                resolved.insert(*name, pointer as usize);
            }
        }
        if !resolved.contains_key("initialize") || !resolved.contains_key("enum_gpus") {
            return Err("NVAPI is present but its entry points are not".into());
        }
        // SAFETY: the address was resolved for NvAPI_Initialize, which takes
        // nothing and returns a status.
        let initialize: StatusFn = unsafe { std::mem::transmute(resolved["initialize"]) };
        // SAFETY: as above; the call has no arguments.
        if unsafe { initialize() } != 0 {
            return Err("NvAPI_Initialize failed".into());
        }
        let mut handles = [std::ptr::null_mut::<c_void>(); 64];
        let mut count = 0u32;
        // SAFETY: NvAPI_EnumPhysicalGPUs fills an array of 64 handles (its
        // NVAPI_MAX_PHYSICAL_GPUS) and a count; both are live locals.
        let enum_gpus: EnumFn = unsafe { std::mem::transmute(resolved["enum_gpus"]) };
        // SAFETY: see above.
        let status = unsafe { enum_gpus(handles.as_mut_ptr(), &mut count) };
        if status != 0 || count == 0 {
            return Err("no NVIDIA GPU enumerated".into());
        }
        Ok(Nvapi { resolved, gpu: handles[0] as usize })
    }

    /// Call an entry point shaped (handle, uint32*), or return `None`.
    fn unsigned(&self, name: &str) -> Option<u32> {
        let pointer = *self.resolved.get(name)?;
        // SAFETY: every name asked for here is an entry point of the
        // (handle, uint32*) shape, resolved by its interface id.
        let call: UintOutFn = unsafe { std::mem::transmute(pointer) };
        let mut value = 0u32;
        // SAFETY: the handle came from NvAPI_EnumPhysicalGPUs and `value` is
        // a live local.
        let status = unsafe { call(self.gpu as *mut c_void, &mut value) };
        // A negative status is NVAPI reporting the call unsupported on this
        // part; the out parameter is meaningless then and must not be used.
        if status != 0 { None } else { Some(value) }
    }

    /// The core rail in microvolts, or `None`.
    ///
    /// Shaped (handle, struct*) rather than the uint-out shape above, so it
    /// does not go through `unsigned()`. The struct is the layout the open
    /// implementations use: version, flags, eight words this does not read,
    /// the value, and eight more. Version is size with 1 in the high half,
    /// which is how every NVAPI struct carries its version.
    fn core_voltage_uv(&self) -> Option<u32> {
        let pointer = *self.resolved.get("core_voltage")?;
        let mut status = NvapiVoltStatus {
            version: std::mem::size_of::<NvapiVoltStatus>() as u32 | (1 << 16),
            ..NvapiVoltStatus::default()
        };
        // SAFETY: the address was resolved for the core-voltage entry point,
        // of the (handle, struct*) shape.
        let call: StructOutFn = unsafe { std::mem::transmute(pointer) };
        // SAFETY: the handle came from NvAPI_EnumPhysicalGPUs and `status` is
        // a live, versioned block of the size its version word states.
        if unsafe { call(self.gpu as *mut c_void, (&mut status as *mut NvapiVoltStatus).cast()) } != 0 {
            return None;
        }
        if status.value_uv == 0 { None } else { Some(status.value_uv) }
    }

    /// Call an entry point shaped (handle, char*), or return `None`.
    fn text(&self, name: &str) -> Option<String> {
        let pointer = *self.resolved.get(name)?;
        // SAFETY: every name asked for here is an entry point of the
        // (handle, char*) shape, resolved by its interface id.
        let call: TextOutFn = unsafe { std::mem::transmute(pointer) };
        let mut buffer = [0u8; 64];
        // SAFETY: NVAPI's short strings are NvAPI_ShortString, 64 bytes,
        // which is what `buffer` holds.
        if unsafe { call(self.gpu as *mut c_void, buffer.as_mut_ptr().cast()) } != 0 {
            return None;
        }
        let text = c_text(&buffer);
        if text.is_empty() { None } else { Some(text) }
    }
}

/// nvmlMemory_t and nvmlBAR1Memory_t: both are three 64-bit totals.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

const _: () = assert!(std::mem::size_of::<NvmlMemory>() == 24);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NvmlUtilization {
    gpu: u32,
    memory: u32,
}

const _: () = assert!(std::mem::size_of::<NvmlUtilization>() == 8);

type NvmlStatusFn = unsafe extern "C" fn() -> i32;
type NvmlSystemTextFn = unsafe extern "C" fn(*mut c_char, u32) -> i32;
type NvmlHandleByIndexFn = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
type NvmlDeviceUintFn = unsafe extern "C" fn(*mut c_void, *mut u32) -> i32;
type NvmlDeviceKindUintFn = unsafe extern "C" fn(*mut c_void, i32, *mut u32) -> i32;
type NvmlDeviceTextFn = unsafe extern "C" fn(*mut c_void, *mut c_char, u32) -> i32;
type NvmlDeviceMemoryFn = unsafe extern "C" fn(*mut c_void, *mut NvmlMemory) -> i32;
type NvmlDeviceUtilizationFn = unsafe extern "C" fn(*mut c_void, *mut NvmlUtilization) -> i32;

/// The loaded NVML library.
struct Nvml {
    library: &'static Library,
}

impl Nvml {
    /// An export by name. A missing one is an error, as the attribute lookup
    /// raised in the Python: callers decide how far that error reaches.
    fn function<T: Copy>(&self, name: &str) -> Result<T, String> {
        symbol(self.library, name).ok_or_else(|| format!("nvml.dll has no {name}"))
    }
}

/// The NVML library, or `None`.
///
/// On PATH for a normal driver install, and under NVSMI for the older
/// layout that did not put it there.
fn load_nvml() -> Option<Nvml> {
    if cfg!(test) {
        // Unit tests never load a vendor library.
        return None;
    }
    for name in ["nvml.dll", "C:\\Program Files\\NVIDIA Corporation\\NVSMI\\nvml.dll"] {
        if let Ok(library) = load_library(name) {
            return Some(Nvml { library });
        }
    }
    None
}

/// What NVML reports about the first GPU.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NvmlFacts {
    pub driver_version: Option<String>,
    pub architecture: Option<u32>,
    pub vbios_version: Option<String>,
    /// The fastest link the card and slot can make: (generation, width).
    pub pcie_link: Option<(u32, u32)>,
    pub bar1_total: Option<u64>,
    pub frame_buffer_total: Option<u64>,
}

pub const NVML_VBIOS_LENGTH: usize = 32;

/// What NVML reports about the first GPU.
///
/// Opened once for everything wanted from it: each init/shutdown pair costs
/// more than all the calls between them.
///
/// An error only when the library is there and has no nvmlInit_v2: that
/// lookup sat outside the Python's try, so it failed the whole card read.
fn nvml_query() -> Result<NvmlFacts, String> {
    #[cfg(test)]
    {
        let faked = TEST_NVML.with(|slot| slot.borrow().as_ref().map(|answer| answer()));
        if let Some(answer) = faked {
            return answer;
        }
    }
    let Some(nvml) = load_nvml() else {
        return Ok(NvmlFacts::default());
    };
    let init: NvmlStatusFn = nvml.function("nvmlInit_v2")?;
    // SAFETY: nvmlInit_v2 takes nothing and returns a status.
    if unsafe { init() } != 0 {
        return Ok(NvmlFacts::default());
    }
    let mut found = NvmlFacts::default();
    // An NVML too old for one of these exports raises rather than returning
    // a status. Whatever was gathered before that still stands.
    let _ = nvml_gather(&nvml, &mut found);
    if let Ok(shutdown) = nvml.function::<NvmlStatusFn>("nvmlShutdown") {
        // SAFETY: paired with the successful nvmlInit_v2 above.
        unsafe {
            shutdown();
        }
    }
    Ok(found)
}

/// The body of [`nvml_query`]: stops at the first missing export.
fn nvml_gather(nvml: &Nvml, found: &mut NvmlFacts) -> Result<(), String> {
    let mut version = [0u8; 80];
    let get_version: NvmlSystemTextFn = nvml.function("nvmlSystemGetDriverVersion")?;
    // SAFETY: the buffer holds the 80 bytes the length argument states.
    if unsafe { get_version(version.as_mut_ptr().cast(), 80) } == 0 {
        let text = c_text(&version);
        if !text.is_empty() {
            found.driver_version = Some(text);
        }
    }

    let mut handle: *mut c_void = std::ptr::null_mut();
    let by_index: NvmlHandleByIndexFn = nvml.function("nvmlDeviceGetHandleByIndex_v2")?;
    // SAFETY: `handle` is a live local that receives an nvmlDevice_t.
    if unsafe { by_index(0, &mut handle) } != 0 {
        return Ok(());
    }

    let mut architecture = 0u32;
    let get_architecture: NvmlDeviceUintFn = nvml.function("nvmlDeviceGetArchitecture")?;
    // SAFETY: a device handle NVML gave out, and a live out parameter.
    if unsafe { get_architecture(handle, &mut architecture) } == 0 {
        found.architecture = Some(architecture);
    }

    let mut vbios = [0u8; NVML_VBIOS_LENGTH];
    let get_vbios: NvmlDeviceTextFn = nvml.function("nvmlDeviceGetVbiosVersion")?;
    // SAFETY: the buffer holds the NVML_VBIOS_LENGTH bytes the call is told.
    if unsafe { get_vbios(handle, vbios.as_mut_ptr().cast(), NVML_VBIOS_LENGTH as u32) } == 0 {
        let text = c_text(&vbios);
        if !text.is_empty() {
            found.vbios_version = Some(text);
        }
    }

    // The fastest link this card and this slot can make between them -- a
    // Gen 5 card in a Gen 4 slot reports 4 -- rather than the current one,
    // which drops to Gen 1 whenever the card idles.
    let (mut generation, mut width) = (0u32, 0u32);
    let max_generation: NvmlDeviceUintFn = nvml.function("nvmlDeviceGetMaxPcieLinkGeneration")?;
    // SAFETY: a device handle NVML gave out, and a live out parameter.
    if unsafe { max_generation(handle, &mut generation) } == 0 {
        let max_width: NvmlDeviceUintFn = nvml.function("nvmlDeviceGetMaxPcieLinkWidth")?;
        // SAFETY: as above.
        if unsafe { max_width(handle, &mut width) } == 0 && generation != 0 && width != 0 {
            found.pcie_link = Some((generation, width));
        }
    }

    // Resizable BAR, without touching a BAR register. Sizing one the usual
    // way means writing all-ones to it, which is not something to do to a
    // live display; the aperture NVML reports says the same thing. Off, it
    // is the legacy 256 MB window whatever the card holds. On, it spans
    // the frame buffer.
    let (mut bar1, mut memory) = (NvmlMemory::default(), NvmlMemory::default());
    let bar1_info: NvmlDeviceMemoryFn = nvml.function("nvmlDeviceGetBAR1MemoryInfo")?;
    // SAFETY: a device handle NVML gave out; nvmlBAR1Memory_t is three u64s.
    if unsafe { bar1_info(handle, &mut bar1) } == 0 {
        let memory_info: NvmlDeviceMemoryFn = nvml.function("nvmlDeviceGetMemoryInfo")?;
        // SAFETY: as above; nvmlMemory_t is three u64s.
        if unsafe { memory_info(handle, &mut memory) } == 0 && bar1.total != 0 && memory.total != 0 {
            found.bar1_total = Some(bar1.total);
            found.frame_buffer_total = Some(memory.total);
        }
    }
    Ok(())
}

/// "PCIe 5.0 x16", the way GPU-Z's bus interface line writes it.
pub fn pcie_link_text(generation: u32, width: u32) -> String {
    format!("PCIe {generation}.0 x{width}")
}

fn named(table: &[(u32, &str)], code: Option<u32>, prefix: &str) -> Option<String> {
    let code = code?;
    Some(
        table
            .iter()
            .find(|(known, _)| *known == code)
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| format!("{prefix} {code}")),
    )
}

/// What the card reports, as a map of display strings.
///
/// Cached: none of it changes while the machine runs, and the callers are row
/// getters that run at startup and again on every Advanced window tick.
/// Missing keys mean the card did not answer, and callers show nothing rather
/// than a placeholder.
///
/// `pnp_device_ids` are the display adapters' PNP ids when the caller has
/// them already; `None` asks WMI.
pub fn read_gpu(refresh: bool, pnp_device_ids: Option<&[String]>) -> GpuInfo {
    if !refresh {
        if let Some(cached) = CACHE.get() {
            return cached;
        }
    }
    let found = read_gpu_uncached(pnp_device_ids).unwrap_or_default();
    // Written on every exit, including the failed ones. Cached only on success,
    // a machine whose driver lacks one export would repeat the whole probe --
    // two DLL loads and an NVAPI init -- for all thirteen rows, every second.
    CACHE.set(found.clone());
    found
}

/// Forget the cached card (`_CACHE[:] = []`), for tests and identity resets.
pub fn clear_cache() {
    CACHE.clear();
}

fn read_gpu_uncached(pnp_device_ids: Option<&[String]>) -> Result<GpuInfo, String> {
    let mut found = GpuInfo::new();
    let pci = adapter_identity(pnp_device_ids);
    if let Some(pci) = pci {
        if pci.vendor_id != NVIDIA_VENDOR_ID {
            // An AMD or Intel card: the board vendors below are add-in-board
            // makers who ship both brands, and NVAPI will not answer for it, so
            // reporting half a row would be reporting someone else's card.
            return Ok(found);
        }
        found.insert("revision", format!("{:02X}", pci.revision));
        let vendor = pci.subsystem_vendor_id;
        found.insert(
            "board_manufacturer",
            BOARD_VENDORS
                .iter()
                .find(|(id, _)| *id == vendor)
                .map(|(_, name)| name.to_string())
                .unwrap_or_else(|| format!("0x{vendor:04X}")),
        );
    }

    let nvml = nvml_query()?;
    if let Some(version) = nvml.driver_version.filter(|text| !text.is_empty()) {
        found.insert("driver_version", version);
    }
    let node = nvml.architecture.and_then(|architecture| {
        NVML_ARCHITECTURE_NODES.iter().find(|(known, _)| *known == architecture).map(|(_, node)| *node)
    });
    if let Some(node) = node {
        found.insert("technology", node.to_string());
    }
    if let Some(vbios) = nvml.vbios_version.filter(|text| !text.is_empty()) {
        found.insert("vbios_version", vbios);
    }
    if let Some((generation, width)) = nvml.pcie_link.filter(|(generation, _)| *generation != 0) {
        found.insert("pcie_link", pcie_link_text(generation, width));
    }
    if let (Some(bar1), Some(frame_buffer)) = (nvml.bar1_total, nvml.frame_buffer_total) {
        if bar1 != 0 && frame_buffer != 0 {
            // Enabled means the aperture reaches the frame buffer. Compared with
            // room to spare rather than exactly: the aperture is a power of two
            // and the frame buffer is not, so an enabled card can report slightly
            // either side of its own memory size.
            let enabled = bar1 as f64 >= frame_buffer as f64 * 0.9;
            found.insert("resizable_bar", if enabled { "Enabled" } else { "Disabled" }.into());
        }
    }

    let Ok(nvapi) = Nvapi::open() else {
        return Ok(found);
    };

    // The core family, as the driver reports it. The SKU suffix CPU-Z adds
    // ("-250") is not readable anywhere, so it is not shown.
    if let Some(code_name) = nvapi.text("short_name") {
        found.insert("code_name", code_name);
    }
    if let Some(cores) = nvapi.unsigned("core_count").filter(|cores| *cores != 0) {
        found.insert("cores", cores.to_string());
    }
    // Reported in kilobytes, less the part of the frame buffer the driver
    // keeps for itself: 12576256 KB on a 12 GB card, 15.89 GB on a 16 GB one.
    // Shown as the memory the card carries, the whole gigabytes GPU-Z gives
    // (16384 MB); what is held back is well under half a gigabyte.
    if let Some(frame_buffer_kb) = nvapi.unsigned("frame_buffer_kb").filter(|kb| *kb != 0) {
        found.insert(
            "memory_size",
            format!("{} GB", pyfmt::round_int(frame_buffer_kb as f64 / 1048576.0)),
        );
    }
    if let Some(width) = nvapi.unsigned("ram_bus_width").filter(|width| *width != 0) {
        found.insert("bus_width", format!("{width} bits"));
    }
    for (key, table, query, prefix) in [
        ("memory_type", NVAPI_RAM_TYPES, "ram_type", "Type"),
        ("memory_vendor", NVAPI_RAM_MAKERS, "ram_maker", "Maker"),
    ] {
        if let Some(name) = named(table, nvapi.unsigned(query), prefix) {
            found.insert(key, name);
        }
    }
    Ok(found)
}

// --- Live card readings, for the Telemetry window.
//
// Separate from read_gpu above, which answers what the card *is* and is cached
// for the run. These move, so they are read on every tick.
//
// NVML is held open rather than initialised per call: an init/shutdown pair
// costs more than every reading between them, and the window ticks once a
// second. The AM5 side reads the same figures through AMD's own libraries; the
// labels match it wherever the two report the same thing, so a row means one
// thing across both platforms.
pub const NVML_TEMPERATURE_GPU: i32 = 0;
pub const NVML_CLOCK_GRAPHICS: i32 = 0;
pub const NVML_CLOCK_MEM: i32 = 2;
pub const NVML_CLOCK_VIDEO: i32 = 3;

/// An open NVML session on the first card.
struct SensorSession {
    nvml: Nvml,
    /// The nvmlDevice_t, kept as an address.
    handle: usize,
}

/// One live session: unset before the first attempt, `None` once it has
/// failed.
static SENSOR_SESSION: Memo<Option<Arc<SensorSession>>> = Memo::new();
static SENSOR_LOCK: Mutex<()> = Mutex::new(());

/// The session for the first card, or `None`. Opened once.
fn sensor_session() -> Option<Arc<SensorSession>> {
    SENSOR_SESSION.get_or_init(open_sensor_session)
}

fn open_sensor_session() -> Option<Arc<SensorSession>> {
    let nvml = load_nvml()?;
    let init: NvmlStatusFn = nvml.function("nvmlInit_v2").ok()?;
    // SAFETY: nvmlInit_v2 takes nothing and returns a status.
    if unsafe { init() } != 0 {
        return None;
    }
    let by_index: NvmlHandleByIndexFn = nvml.function("nvmlDeviceGetHandleByIndex_v2").ok()?;
    let mut handle: *mut c_void = std::ptr::null_mut();
    // SAFETY: `handle` is a live local that receives an nvmlDevice_t.
    if unsafe { by_index(0, &mut handle) } == 0 {
        return Some(Arc::new(SensorSession { nvml, handle: handle as usize }));
    }
    let shutdown: NvmlStatusFn = nvml.function("nvmlShutdown").ok()?;
    // SAFETY: paired with the successful nvmlInit_v2 above.
    unsafe {
        shutdown();
    }
    None
}

/// Live readings keyed by row name, or empty when the card cannot be read.
///
/// A reading NVML declines is left out rather than zeroed: a card that does
/// not report a fan is not a card whose fan is stopped.
pub fn read_gpu_sensors() -> GpuSensors {
    let _guard = SENSOR_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(session) = sensor_session() else {
        return GpuSensors::new();
    };
    let mut found = GpuSensors::new();
    if read_session_sensors(&session, &mut found).is_err() {
        // An export missing from this NVML ends the reading where it stood.
        return found;
    }

    // Core voltage is NVAPI's, not NVML's. Its own session, opened once
    // and kept: a card whose driver has NVML but not NVAPI still gets
    // everything above.
    if let Some(microvolts) = voltage_session_read().filter(|uv| *uv != 0) {
        found.insert("GPU Core Voltage", format!("{:.3} V", microvolts as f64 / 1e6));
    }
    found
}

fn read_session_sensors(session: &SensorSession, found: &mut GpuSensors) -> Result<(), String> {
    let nvml = &session.nvml;
    let handle = session.handle as *mut c_void;
    let unsigned = |call: NvmlDeviceUintFn| {
        let mut value = 0u32;
        // SAFETY: a device handle NVML gave out, and a live out parameter.
        if unsafe { call(handle, &mut value) } == 0 { Some(value) } else { None }
    };
    let unsigned_of = |call: NvmlDeviceKindUintFn, kind: i32| {
        let mut value = 0u32;
        // SAFETY: as above, with the sensor or clock selector NVML defines.
        if unsafe { call(handle, kind, &mut value) } == 0 { Some(value) } else { None }
    };

    let temperature = unsigned_of(nvml.function("nvmlDeviceGetTemperature")?, NVML_TEMPERATURE_GPU);
    if let Some(temperature) = temperature {
        found.insert("GPU Temperature", format!("{temperature} °C"));
    }

    for (label, clock) in [
        ("GPU Clock", NVML_CLOCK_GRAPHICS),
        ("GPU Memory Clock", NVML_CLOCK_MEM),
        ("GPU Video Clock", NVML_CLOCK_VIDEO),
    ] {
        if let Some(megahertz) = unsigned_of(nvml.function("nvmlDeviceGetClockInfo")?, clock) {
            found.insert(label, format!("{megahertz} MHz"));
        }
    }

    // Milliwatts from NVML both times. Board power is what the card
    // is drawing; the limit is what it is allowed, and the pair is
    // only useful together -- which is why AM5 carries the limit too.
    if let Some(milliwatts) = unsigned(nvml.function("nvmlDeviceGetPowerUsage")?) {
        found.insert("GPU Board Power (TBP)", format!("{:.1} W", milliwatts as f64 / 1000.0));
    }
    if let Some(limit) = unsigned(nvml.function("nvmlDeviceGetEnforcedPowerLimit")?) {
        found.insert("GPU Board Power Limit", format!("{:.0} W", limit as f64 / 1000.0));
    }

    // NVML reports the fan as a percentage of its range. AM5 has both
    // RPM and PWM from ADL; this card exposes only the percentage, so
    // it takes the PWM name rather than claiming an RPM it never read.
    if let Some(fan) = unsigned(nvml.function("nvmlDeviceGetFanSpeed")?) {
        found.insert("GPU Fan PWM", format!("{fan} %"));
    }

    let mut utilization = NvmlUtilization::default();
    let get_utilization: NvmlDeviceUtilizationFn = nvml.function("nvmlDeviceGetUtilizationRates")?;
    // SAFETY: a device handle NVML gave out; nvmlUtilization_t is two u32s.
    if unsafe { get_utilization(handle, &mut utilization) } == 0 {
        found.insert("GPU Utilization", format!("{} %", utilization.gpu));
    }

    let mut memory = NvmlMemory::default();
    let get_memory: NvmlDeviceMemoryFn = nvml.function("nvmlDeviceGetMemoryInfo")?;
    // SAFETY: a device handle NVML gave out; nvmlMemory_t is three u64s.
    if unsafe { get_memory(handle, &mut memory) } == 0 {
        found.insert("GPU Memory Used", format!("{} MB", memory.used / 1048576));
    }
    Ok(())
}

/// The NVAPI session the voltage row uses: unset before the first attempt,
/// `None` once it has failed, so a card without it is asked once rather than
/// every tick.
static VOLTAGE_SESSION: Memo<Option<Arc<Nvapi>>> = Memo::new();

fn voltage_session_read() -> Option<u32> {
    let session = VOLTAGE_SESSION.get_or_init(|| Nvapi::open().ok().map(Arc::new))?;
    session.core_voltage_uv()
}

#[cfg(test)]
mod tests {
    //! Cover the graphics rows, and which of them are readings.
    //!
    //! Everything nvidia_gpu shows is asked of the card. ROP/TMU counts and the
    //! chip's SKU have no entry point on this driver, so they are not shown at
    //! all; these tests pin that, because a table that answers for a card reads
    //! exactly like a measurement.

    use super::*;
    use std::cell::{Cell, RefCell};

    const BENCH_CARD: u16 = 0x2786; // AD104, the RTX 4070 on the bench
    const OTHER_CARD: u16 = 0x1B80; // Pascal, a GTX 1080
    const GIGABYTE: u16 = 0x1458;

    #[derive(Clone, Copy)]
    struct Card {
        device: u16,
        subsystem: u16,
        revision: u8,
        vendor: u16,
    }

    fn card() -> Card {
        Card { device: BENCH_CARD, subsystem: GIGABYTE, revision: 0xA1, vendor: NVIDIA_VENDOR_ID }
    }

    /// The card as Windows names it, which is what the identity is read from.
    fn pnp(card: Option<Card>) -> Vec<String> {
        card.map(|c| {
            format!(
                "PCI\\VEN_{:04X}&DEV_{:04X}&SUBSYS_40C6{:04X}&REV_{:02X}\\4&256A0AA8&0&0008",
                c.vendor, c.device, c.subsystem, c.revision
            )
        })
        .into_iter()
        .collect()
    }

    // --- A stand-in for nvapi_QueryInterface: the entry points it resolves
    // write what the test thread put in these cells.

    thread_local! {
        static SHORT_NAME: RefCell<Option<&'static str>> = const { RefCell::new(None) };
        static UNSIGNED: RefCell<BTreeMap<&'static str, u32>> = const { RefCell::new(BTreeMap::new()) };
        static VOLTAGE_VERSION: Cell<u32> = const { Cell::new(0) };
    }

    const FAKE_GPU: usize = 0x6000;

    unsafe extern "C" fn fake_initialize() -> i32 {
        0
    }

    unsafe extern "C" fn fake_enum(handles: *mut *mut c_void, count: *mut u32) -> i32 {
        // SAFETY: the caller passes its 64-handle array and a count.
        unsafe {
            *handles = FAKE_GPU as *mut c_void;
            *count = 1;
        }
        0
    }

    unsafe extern "C" fn fake_short_name(gpu: *mut c_void, buffer: *mut c_char) -> i32 {
        assert_eq!(gpu as usize, FAKE_GPU);
        let Some(name) = SHORT_NAME.with(|n| *n.borrow()) else { return -1 };
        // SAFETY: the caller's buffer is 64 bytes and the name is shorter.
        unsafe {
            std::ptr::copy_nonoverlapping(name.as_ptr(), buffer.cast::<u8>(), name.len());
            *buffer.add(name.len()) = 0;
        }
        0
    }

    unsafe extern "C" fn fake_unsigned<const INDEX: usize>(gpu: *mut c_void, out: *mut u32) -> i32 {
        assert_eq!(gpu as usize, FAKE_GPU);
        let name = NVAPI_QUERIES[INDEX].0;
        match UNSIGNED.with(|values| values.borrow().get(name).copied()) {
            // SAFETY: the caller passes a live u32.
            Some(value) => unsafe {
                *out = value;
                0
            },
            // NVAPI_NOT_SUPPORTED.
            None => -104,
        }
    }

    unsafe extern "C" fn fake_voltage(gpu: *mut c_void, status: *mut c_void) -> i32 {
        assert_eq!(gpu as usize, FAKE_GPU);
        let status = status.cast::<NvapiVoltStatus>();
        // SAFETY: the caller passes its versioned status block.
        unsafe {
            VOLTAGE_VERSION.with(|v| v.set((*status).version));
            (*status).value_uv = 960_000;
        }
        0
    }

    unsafe extern "C" fn fake_query(identifier: u32) -> *mut c_void {
        let index = NVAPI_QUERIES.iter().position(|(_, id)| *id == identifier);
        let address: usize = match index {
            Some(0) => fake_initialize as StatusFn as usize,
            Some(1) => fake_enum as EnumFn as usize,
            Some(3) => fake_short_name as TextOutFn as usize,
            Some(2) => fake_unsigned::<2> as UintOutFn as usize,
            Some(4) => fake_unsigned::<4> as UintOutFn as usize,
            Some(5) => fake_unsigned::<5> as UintOutFn as usize,
            Some(6) => fake_unsigned::<6> as UintOutFn as usize,
            Some(7) => fake_unsigned::<7> as UintOutFn as usize,
            Some(8) => fake_voltage as StructOutFn as usize,
            _ => 0,
        };
        address as *mut c_void
    }

    /// The card's NVAPI, answering `short_name` and the `unsigned` values.
    fn nvapi(short_name: Option<&'static str>, values: &[(&'static str, u32)]) {
        SHORT_NAME.with(|n| *n.borrow_mut() = short_name);
        UNSIGNED.with(|v| *v.borrow_mut() = values.iter().copied().collect());
        TEST_NVAPI.with(|slot| slot.set(Some(fake_query)));
    }

    fn nvml(facts: NvmlFacts) {
        TEST_NVML.with(|slot| *slot.borrow_mut() = Some(Box::new(move || Ok(facts.clone()))));
    }

    /// Read the card as it would be on a described machine.
    ///
    /// NVAPI is off unless a test installs it: it cannot be faked usefully
    /// for most of these, and every value it supplies is already covered by
    /// the values it does not.
    fn read(pci: Option<Card>, facts: NvmlFacts) -> GpuInfo {
        clear_cache();
        nvml(facts);
        read_gpu(true, Some(&pnp(pci)))
    }

    // --- UnitCountTest

    #[test]
    fn the_bench_card_reports_what_cpuz_reports() {
        let found = read(Some(card()), NvmlFacts { architecture: Some(8), ..Default::default() });
        assert_eq!(found["technology"], "4 nm");
        assert_eq!(found["revision"], "A1");
        assert_eq!(found["board_manufacturer"], "GIGABYTE Technology");
    }

    #[test]
    fn nothing_is_claimed_from_the_device_id_alone() {
        // Unit counts and a SKU have no entry point to read them from, so a
        // card is never answered for from a table keyed on its id: with NVAPI
        // down there is no code name at all. (The Python also checked that no
        // GPU_DEVICE_TABLE exists; here that is the module not declaring one.)
        for device in [BENCH_CARD, OTHER_CARD] {
            let found = read(Some(Card { device, ..card() }), NvmlFacts::default());
            assert!(!found.contains_key("rops_tmus"), "device 0x{device:X}");
            assert!(!found.contains_key("code_name"), "device 0x{device:X}");
        }
    }

    #[test]
    fn the_code_name_is_the_one_the_driver_reports() {
        nvapi(Some("GB203"), &[]);
        let found = read(Some(card()), NvmlFacts::default());
        assert_eq!(found["code_name"], "GB203");
    }

    #[test]
    fn the_memory_reads_as_the_card_carries_it() {
        // This RTX 5070 Ti: what the driver leaves usable is 15.89 GB of its
        // 16, and GPU-Z reads the codes beside it as "GDDR7 (Hynix)".
        nvapi(None, &[("frame_buffer_kb", 16662528), ("ram_type", 16), ("ram_maker", 6)]);
        let found = read(Some(card()), NvmlFacts::default());
        assert_eq!(found["memory_size"], "16 GB");
        assert_eq!(found["memory_type"], "GDDR7");
        assert_eq!(found["memory_vendor"], "SK hynix");
    }

    #[test]
    fn an_unlisted_board_vendor_prints_its_id() {
        let found = read(Some(Card { subsystem: 0x1234, ..card() }), NvmlFacts::default());
        assert_eq!(found["board_manufacturer"], "0x1234");
    }

    #[test]
    fn no_card_at_all_is_an_empty_answer_rather_than_an_error() {
        assert_eq!(read(None, NvmlFacts::default()), GpuInfo::new());
    }

    #[test]
    fn another_vendors_card_is_not_reported_as_this_one() {
        // The board-vendor table is add-in-board makers, who ship both brands,
        // so an AMD card would otherwise pick up a plausible-looking vendor and
        // revision with nothing behind them.
        assert_eq!(read(Some(Card { vendor: 0x1002, ..card() }), NvmlFacts::default()), GpuInfo::new());
    }

    // --- CacheTest

    #[test]
    fn a_failed_read_is_cached_too() {
        // Cached only on success, a driver missing one export would repeat two
        // DLL loads and an NVAPI init for all thirteen rows, every second.
        thread_local! { static CALLS: Cell<u32> = const { Cell::new(0) }; }
        TEST_NVML.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(|| {
                CALLS.with(|calls| calls.set(calls.get() + 1));
                Err("nvml.dll has no nvmlInit_v2".into())
            }))
        });
        clear_cache();
        assert_eq!(read_gpu(false, Some(&[])), GpuInfo::new());
        assert_eq!(read_gpu(false, Some(&[])), GpuInfo::new());
        assert_eq!(CALLS.with(Cell::get), 1);
        assert_eq!(CACHE.get(), Some(GpuInfo::new()));
    }

    #[test]
    fn a_refresh_replaces_the_cache_rather_than_growing_it() {
        // It appended, so the list grew on every refresh and _CACHE[0] kept
        // handing back the first answer.
        read(Some(card()), NvmlFacts::default());
        nvml(NvmlFacts { architecture: Some(8), ..Default::default() });
        let second = read_gpu(true, Some(&pnp(Some(card()))));
        assert_eq!(CACHE.get(), Some(second.clone()));
        assert_eq!(read_gpu(false, None)["technology"], "4 nm");
    }

    // --- ResizableBarTest: the aperture is the reading; Enabled/Disabled is
    // what it means.

    fn bar(bar1_total: u64, frame_buffer_total: u64) -> Option<String> {
        read(
            None,
            NvmlFacts {
                bar1_total: Some(bar1_total),
                frame_buffer_total: Some(frame_buffer_total),
                ..Default::default()
            },
        )
        .get("resizable_bar")
        .cloned()
    }

    #[test]
    fn the_legacy_window_reads_as_disabled() {
        // 256 MB against a 12 GB card: what this bench reports today.
        assert_eq!(bar(268435456, 12878610432).as_deref(), Some("Disabled"));
    }

    #[test]
    fn an_aperture_spanning_the_frame_buffer_reads_as_enabled() {
        assert_eq!(bar(12884901888, 12878610432).as_deref(), Some("Enabled"));
    }

    #[test]
    fn an_aperture_just_under_the_frame_buffer_still_reads_enabled() {
        // The aperture is a power of two and the frame buffer is not, so an
        // enabled card can report slightly under its own memory size. Exact
        // comparison would call that disabled.
        assert_eq!(bar(12884901888, 13958643712).as_deref(), Some("Enabled"));
    }

    #[test]
    fn no_aperture_reported_means_no_row() {
        assert_eq!(bar(0, 12878610432), None);
    }

    // --- MeasuredTableTest

    #[test]
    fn only_measured_memory_codes_are_named() {
        // These two enumerations are unpublished, and the values measured here
        // contradicted the ordering they are usually quoted with. Filling in
        // the rest from memory is the mistake this guards.
        assert_eq!(NVAPI_RAM_TYPES, &[(15, "GDDR6X"), (16, "GDDR7")]);
        assert_eq!(NVAPI_RAM_MAKERS, &[(6, "SK hynix"), (10, "Micron")]);
    }

    #[test]
    fn an_unmeasured_code_prints_itself() {
        assert_eq!(named(NVAPI_RAM_TYPES, Some(9), "Type").as_deref(), Some("Type 9"));
        assert_eq!(named(NVAPI_RAM_TYPES, None, "Type"), None);
    }

    // --- AdapterIdentityTest: the PNP device id carries what the PCI bus scan
    // used to walk for.

    #[test]
    fn the_bench_adapter_decodes_to_its_pci_identity() {
        let found = adapter_identity(Some(&[
            r"PCI\VEN_10DE&DEV_2786&SUBSYS_40C61458&REV_A1\4&256A0AA8&0&0008".to_string(),
        ]))
        .unwrap();
        assert_eq!(found.vendor_id, 0x10DE);
        assert_eq!(found.device_id, 0x2786);
        // SUBSYS is device then vendor: 40C6 is the board, 1458 GIGABYTE.
        assert_eq!(found.subsystem_vendor_id, 0x1458);
        assert_eq!(found.subsystem_device_id, 0x40C6);
        assert_eq!(found.revision, 0xA1);
    }

    #[test]
    fn an_id_that_is_not_a_pci_device_is_ignored() {
        assert_eq!(adapter_identity(Some(&["ROOT\\BASICDISPLAY".to_string()])), None);
        assert_eq!(adapter_identity(Some(&[String::new()])), None);
        assert_eq!(adapter_identity(Some(&[])), None);
    }

    // --- Beyond the Python tests: the NVAPI layer itself, through a stand-in
    // QueryInterface, so the shapes and the voltage block's version word are
    // pinned.

    #[test]
    fn nvapi_resolves_by_interface_id_and_reads_through_the_shapes() {
        nvapi(Some("  AD104 "), &[("core_count", 5888), ("ram_bus_width", 192)]);
        let session = Nvapi::open().unwrap();
        assert_eq!(session.text("short_name").as_deref(), Some("AD104"));
        assert_eq!(session.unsigned("core_count"), Some(5888));
        assert_eq!(session.unsigned("ram_type"), None);
        assert_eq!(session.core_voltage_uv(), Some(960_000));
        assert_eq!(VOLTAGE_VERSION.with(Cell::get), 76 | (1 << 16));
    }

    #[test]
    fn without_nvapi_the_card_is_what_the_pnp_id_and_nvml_say() {
        let found = read(
            Some(card()),
            NvmlFacts {
                driver_version: Some("576.02".into()),
                vbios_version: Some("95.04.31.00.A1".into()),
                pcie_link: Some((4, 16)),
                ..Default::default()
            },
        );
        assert_eq!(found["driver_version"], "576.02");
        assert_eq!(found["vbios_version"], "95.04.31.00.A1");
        assert_eq!(found["pcie_link"], "PCIe 4.0 x16");
        assert!(!found.contains_key("cores"));
    }
}
