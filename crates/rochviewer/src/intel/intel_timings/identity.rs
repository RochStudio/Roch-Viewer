//! What the machine is: the CPU, the board, the OS, the card and the modules.
//!
//! Identity, not telemetry. None of these can return a different answer while
//! the machine is running, and most cost a WMI query, so they are read once
//! and cached; see the note above `get_cpu_name`.

use std::sync::LazyLock;

use regex::Regex;

use crate::hardware::pci_mcfg::ecam_address;
use crate::hardware::pci_mcfg::McfgAllocation;
use crate::hardware::read::read_physical_memory_int;
use crate::memo::Memo;
use crate::memory::ddr_generation::{DdrGeneration, detect_ddr_generation};
use crate::platform_profiles::Platform;
use crate::pyfmt::round_int;
use crate::system_identity::{self, RegValue, py_int, py_strip};
use crate::wmi_util::{WmiObject, WmiValue, static_class};

// Intel client CPUID models, and the process node each is built on. Family 6
// throughout; the model is the extended CPUID byte.
//
// 0xB7 is what this bench reports and is confirmed against CPU-Z, which names
// it Raptor Lake on 10 nm. The rest are the other LGA1700 silicon this project
// targets. Anything unlisted prints its family and model rather than a guess,
// the same way an unlisted JEP106 vendor prints its raw ID.
pub const INTEL_CODE_NAMES: &[(u32, (&str, &str))] = &[
    (0x97, ("Alder Lake", "10 nm")),
    (0x9A, ("Alder Lake", "10 nm")),
    (0xBE, ("Alder Lake", "10 nm")),
    (0xB7, ("Raptor Lake", "10 nm")),
    (0xBA, ("Raptor Lake", "10 nm")),
    (0xBF, ("Raptor Lake", "10 nm")),
    // Arrow Lake-S, CPUID 06_C6H. Measured on a Core Ultra 7 270K Plus, where
    // the fallback printed "Family 6 Model 0xC6" in Code Name and left
    // Technology blank.
    //
    // The node names the compute tile only. Unlike the monolithic parts above,
    // this is a disaggregated package whose tiles are built on different
    // processes, so a single unqualified figure would be wrong for most of the
    // silicon the row appears to describe.
    (0xC6, ("Arrow Lake", "3 nm (compute tile)")),
];

// PCH LPC/eSPI controller device IDs at 00:1F.0. 0x7A04 is measured on this
// board, where CPU-Z names it Z790. Unlisted parts print the device ID.
pub const PCH_DEVICE_NAMES: &[(u16, &str)] = &[(0x7A04, "Z790")];

// SMBIOS Type 17 memory types (SMBIOSMemoryType), which is where the DDR
// generation is stated. WMI's own MemoryType field reads 0 on this board.
//
// The low-power codes are here because detect_ddr_generation already treats
// them as their generation; without them an LPDDR5 machine printed "Type 35"
// in this row while the rest of the app called the same modules DDR5.
pub const SMBIOS_MEMORY_TYPES: &[(i64, &str)] = &[
    (20, "DDR"),
    (21, "DDR2"),
    (24, "DDR3"),
    (26, "DDR4"),
    (30, "LPDDR4"),
    (34, "DDR5"),
    (35, "LPDDR5"),
];

/// The first object of a WMI class, read once for the process.
fn first(class_name: &str) -> Option<WmiObject> {
    static_class(class_name).first().cloned()
}

/// `(getattr(obj, field, "") or "").strip()`.
fn stripped(object: &WmiObject, field: &str) -> String {
    py_strip(&object.str_or_empty(field)).to_string()
}

/// Python truthiness of a WMI value: `value or default` falls through on these.
fn truthy(value: &WmiValue) -> bool {
    match value {
        WmiValue::Null => false,
        WmiValue::Bool(b) => *b,
        WmiValue::Int(i) => *i != 0,
        WmiValue::UInt(u) => *u != 0,
        WmiValue::Float(f) => *f != 0.0,
        WmiValue::Str(s) => !s.is_empty(),
        WmiValue::Array(items) => !items.is_empty(),
    }
}

/// `int(getattr(obj, field, 0) or 0)`: `Err` where `int()` would raise.
fn int_or_zero(object: &WmiObject, field: &str) -> Result<i64, String> {
    match object.get(field).filter(|value| truthy(value)) {
        None => Ok(0),
        Some(value) => value
            .to_i64()
            .ok_or_else(|| format!("invalid literal for int(): {:?}", value.to_text())),
    }
}

static ECAM_ALLOCATION: Memo<Option<McfgAllocation>> = Memo::new();

fn ecam_allocation() -> Option<McfgAllocation> {
    ECAM_ALLOCATION.get_or_init(|| crate::intel::intel_pch_smbus::default_ecam_allocation().ok())
}

/// Read one dword from a PCI function's configuration space.
fn pci_config_dword(device: u8, function: u8, offset: u16) -> Option<u64> {
    let allocation = ecam_allocation()?;
    let address = ecam_address(&allocation, 0, device, function, offset).ok()?;
    read_physical_memory_int(address, 4)
}

static INTEL_SILICON: Memo<Option<(Option<String>, Option<String>)>> = Memo::new();
static CHIPSET_NAME: Memo<Option<String>> = Memo::new();
static SOUTHBRIDGE_NAME: Memo<Option<String>> = Memo::new();
static LPCIO_NAME: Memo<Option<String>> = Memo::new();
static MEMORY_TYPE: Memo<Option<String>> = Memo::new();
static OS_NAME: Memo<Option<String>> = Memo::new();
static WINDOWS_VERSION_VALUES: Memo<std::collections::BTreeMap<&'static str, RegValue>> = Memo::new();
static CPU_NAME: Memo<String> = Memo::new();
static CORE_TYPES: Memo<Option<String>> = Memo::new();
static MOTHERBOARD_VERSION: Memo<Option<String>> = Memo::new();

/// Forget every cached platform reading. For tests and probes.
pub fn clear_identity_caches() {
    ECAM_ALLOCATION.clear();
    INTEL_SILICON.clear();
    CHIPSET_NAME.clear();
    SOUTHBRIDGE_NAME.clear();
    LPCIO_NAME.clear();
    MEMORY_TYPE.clear();
    OS_NAME.clear();
    WINDOWS_VERSION_VALUES.clear();
}

fn cpu_family_model() -> Option<Result<(u32, u32), String>> {
    // From system_identity, not from the probe that first defined it: that
    // module is a write-capable research tool and importing it here shipped
    // it inside the release EXE.
    let cpu = first("Win32_Processor")?;
    Some(system_identity::decode_wmi_processor_id(cpu.text("ProcessorId").as_deref()))
}

/// `(code name, process node)` for this CPU.
///
/// The node is a property of the silicon with nothing on the machine to read
/// it from, so it is only reported for a part the table names: an unlisted
/// one gets a code name built from its CPUID and no node at all, rather than
/// inheriting its neighbour's.
fn intel_silicon() -> Option<(Option<String>, Option<String>)> {
    INTEL_SILICON.get_or_init(|| {
        let found = match cpu_family_model() {
            None => return Some((None, None)),
            // A decode that raised is cached as no reading.
            Some(Err(_)) => return None,
            Some(Ok(found)) => found,
        };
        let (family, model) = found;
        if family == 0x06
            && let Some((_, (name, node))) = INTEL_CODE_NAMES.iter().find(|(m, _)| *m == model) {
                return Some((Some(name.to_string()), Some(node.to_string())));
            }
        Some((Some(format!("Family {family} Model 0x{model:02X}")), None))
    })
}

/// The silicon's code name, from CPUID rather than the marketing string.
pub fn get_cpu_codename() -> Option<String> {
    intel_silicon().and_then(|(name, _)| name)
}

/// The process node the silicon is built on.
pub fn get_cpu_technology() -> Option<String> {
    intel_silicon().and_then(|(_, node)| node)
}

/// The processor socket/package name reported by SMBIOS through WMI.
pub fn get_cpu_package() -> Option<String> {
    let cpu = first("Win32_Processor")?;
    let value = stripped(&cpu, "SocketDesignation");
    if value.is_empty() { None } else { Some(value) }
}

/// Display CPUID family, model and stepping without a SKU lookup table.
pub fn get_cpu_signature() -> Option<String> {
    let cpu = first("Win32_Processor")?;
    let (family, model, stepping) =
        system_identity::decode_wmi_processor_signature(cpu.text("ProcessorId").as_deref()).ok()?;
    Some(format!("{family} / 0x{model:02X} / {stepping}"))
}

/// `(device id, revision)` for a PCI function, or `None`.
fn pci_device_and_revision(device: u8, function: u8) -> Option<(u64, u64)> {
    let identity = pci_config_dword(device, function, 0x00)?;
    if identity == 0xFFFF_FFFF || identity == 0 {
        return None;
    }
    let revision = pci_config_dword(device, function, 0x08).unwrap_or(0) & 0xFF;
    Some(((identity >> 16) & 0xFFFF, revision))
}

/// The host bridge: the code name of the silicon, and its revision.
///
/// CPU-Z calls this the Northbridge on its report and Chipset in its window.
/// The name comes from CPUID -- on client parts the host bridge is the same
/// die as the cores -- and the revision from 00:00.0, so both halves are
/// read rather than assumed.
pub fn get_chipset_name() -> Option<String> {
    CHIPSET_NAME.get_or_init(|| {
        let (device, revision) = pci_device_and_revision(0x00, 0)?;
        let name = match get_cpu_codename() {
            Some(name) if !name.is_empty() && !name.starts_with("Family ") => name,
            _ => format!("Host Bridge 0x{device:04X}"),
        };
        Some(format!("Intel {name} rev. {revision:02X}"))
    })
}

/// The PCH at 00:1F.0, by device ID, with its revision.
pub fn get_southbridge_name() -> Option<String> {
    SOUTHBRIDGE_NAME.get_or_init(|| {
        let (device, revision) = pci_device_and_revision(0x1F, 0)?;
        let name = PCH_DEVICE_NAMES
            .iter()
            .find(|(id, _)| *id as u64 == device)
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| format!("PCH 0x{device:04X}"));
        Some(format!("Intel {name} rev. {revision:02X}"))
    })
}

/// The Super I/O on the LPC bus, by its chip ID.
///
/// Through the board sensors' own profile, which is the thing that already
/// decided which chip is here. Probing separately would unlock and re-lock
/// the configuration window under the monitoring mutex for an answer that
/// cannot change -- and asking one reader by name gets it wrong: this row
/// read nothing on the Z790-P, whose NCT6687D answers 0xD592 at port 0x4E.
/// That is a Nuvoton, but not one the NCT679x reader knows, so the row went
/// blank on a board whose Sensors tab was reading that very chip.
///
/// The vendor comes from the reader that answered rather than from the chip
/// name, which carries no vendor of its own. (`system_identity::lpcio_name`
/// is the same reading, shared with the AM5 profile.)
pub fn get_lpcio_name() -> Option<String> {
    LPCIO_NAME.get_or_init(system_identity::lpcio_name)
}

/// DDR generation, as SMBIOS states it.
pub fn get_memory_type() -> Option<String> {
    MEMORY_TYPE.get_or_init(|| {
        let mut found: Vec<String> = Vec::new();
        for memory in static_class("Win32_PhysicalMemory").iter() {
            let code = match memory.get("SMBIOSMemoryType") {
                None | Some(WmiValue::Null) => continue,
                // int() on something that is not a number raised, and the
                // cached reading became nothing.
                Some(value) => value.to_i64()?,
            };
            let name = SMBIOS_MEMORY_TYPES
                .iter()
                .find(|(known, _)| *known == code)
                .map(|(_, name)| name.to_string())
                .unwrap_or_else(|| format!("Type {code}"));
            if !found.contains(&name) {
                found.push(name);
            }
        }
        if found.is_empty() { None } else { Some(found.join(" / ")) }
    })
}

// Identity, not telemetry. None of the four functions below can return a
// different answer while the machine is running, and each costs a WMI query of
// roughly a second. Uncached, is_arrow_lake_platform alone was called 26 times
// while the table was being built, each time re-asking Windows for the CPU
// name, which is most of what made starting the viewer take half a minute.
pub fn get_cpu_name() -> String {
    CPU_NAME.get_or_init(|| {
        let name = first("Win32_Processor").and_then(|cpu| cpu.text("Name"));
        match name {
            Some(cpu_name) => {
                // "Intel(R) Core(TM) i5-14600KF" is the registered form; the
                // marks carry no information the reader wants and every other
                // tool drops them. Collapsed afterwards so their removal
                // leaves no double space.
                let mut cpu_name = cpu_name;
                for mark in ["(R)", "(TM)", "Processor"] {
                    cpu_name = cpu_name.replace(mark, "");
                }
                // str.split() also splits on \x1c-\x1f, which Rust's
                // whitespace does not include.
                cpu_name
                    .split(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            None => {
                eprintln!("Error retrieving CPU name: list index out of range");
                "Error".into()
            }
        }
    })
}

pub fn get_total_physical_memory(platform: Platform) -> String {
    let memory_bytes = first("Win32_ComputerSystem").and_then(|system| system.int("TotalPhysicalMemory"));
    match memory_bytes {
        Some(memory_bytes) => {
            let memory_gb = round_int(memory_bytes as f64 / (1024f64 * 1024.0 * 1024.0));
            // A space before the unit on DDR5, as every other value has.
            if platform == Platform::Lga1700Ddr5 {
                format!("{memory_gb} GB")
            } else {
                format!("{memory_gb}GB")
            }
        }
        None => {
            eprintln!("Error retrieving physical memory size: list index out of range");
            "Error".into()
        }
    }
}

pub fn get_motherboard_name() -> Option<String> {
    match first("Win32_BaseBoard").map(|board| board.get("Product").cloned()) {
        Some(Some(product)) => product.to_text(),
        _ => {
            eprintln!("Error retrieving motherboard name: list index out of range");
            Some("Unknown".into())
        }
    }
}

pub fn get_cpu_cores_threads() -> String {
    let Some(cpu) = first("Win32_Processor") else {
        eprintln!("Error retrieving CPU cores/threads: list index out of range");
        return "Unknown".into();
    };
    let cores = cpu.get("NumberOfCores").filter(|v| truthy(v)).and_then(WmiValue::to_text);
    let threads = cpu.get("NumberOfLogicalProcessors").filter(|v| truthy(v)).and_then(WmiValue::to_text);
    match (cores, threads) {
        (Some(cores), Some(threads)) => format!("{cores} / {threads}"),
        _ => "Unknown".into(),
    }
}

// GetLogicalProcessorInformationEx: one record per physical core, whose
// PROCESSOR_RELATIONSHIP carries the core's SMT flag at byte 8 and its
// efficiency class at byte 9.
pub const RELATION_PROCESSOR_CORE: u32 = 0;
pub const LTP_PC_SMT: u8 = 0x1;

/// (efficiency class, has SMT) for each enabled core, or [] if unread.
fn processor_cores() -> Vec<(u8, bool)> {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, LOGICAL_PROCESSOR_RELATIONSHIP,
        SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };

    let relation = LOGICAL_PROCESSOR_RELATIONSHIP(RELATION_PROCESSOR_CORE as i32);
    let mut size: u32 = 0;
    // SAFETY: a size query with no buffer.
    let _ = unsafe { GetLogicalProcessorInformationEx(relation, None, &mut size) };
    if size == 0 {
        return Vec::new();
    }
    // u64 storage keeps the records aligned for the API.
    let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
    // SAFETY: the buffer holds `size` bytes, which is what the API asked for.
    let filled = unsafe {
        GetLogicalProcessorInformationEx(
            relation,
            Some(buffer.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX),
            &mut size,
        )
    };
    if filled.is_err() {
        return Vec::new();
    }
    let data: Vec<u8> = buffer.iter().flat_map(|word| word.to_le_bytes()).take(size as usize).collect();
    let mut cores = Vec::new();
    let mut offset = 0usize;
    while offset + 10 <= data.len() {
        let relationship = u32::from_le_bytes(data[offset..offset + 4].try_into().expect("four bytes"));
        let record_size = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().expect("four bytes"));
        if record_size == 0 {
            break;
        }
        if relationship == RELATION_PROCESSOR_CORE {
            let (flags, efficiency) = (data[offset + 8], data[offset + 9]);
            cores.push((efficiency, flags & LTP_PC_SMT != 0));
        }
        offset += record_size as usize;
    }
    cores
}

/// How many P- and E-cores `cores` holds, as "6P + 8E", or `None`.
///
/// On a hybrid part Windows gives the P-cores the higher efficiency class.
/// With the E-cores off in the BIOS every core shares one class; those cores
/// are all P-cores when every one of them runs two threads, as no E-core on
/// this platform does. With Hyper-Threading off as well nothing tells the two
/// apart, and the row says nothing rather than guess.
pub fn core_types_text(cores: &[(u8, bool)]) -> Option<String> {
    if cores.is_empty() {
        return None;
    }
    let mut classes: Vec<u8> = cores.iter().map(|(efficiency, _)| *efficiency).collect();
    classes.sort_unstable();
    classes.dedup();
    if classes.len() > 1 {
        let top = *classes.iter().max().expect("non-empty");
        let performance = cores.iter().filter(|(efficiency, _)| *efficiency == top).count();
        return Some(format!("{performance}P + {}E", cores.len() - performance));
    }
    if cores.iter().all(|(_, smt)| *smt) {
        return Some(format!("{}P + 0E", cores.len()));
    }
    None
}

/// The enabled P- and E-cores, from Windows' own processor records.
pub fn get_core_types() -> Option<String> {
    CORE_TYPES.get_or_init(|| core_types_text(&processor_cores()))
}

/// Board maker and product on one line -- "ASUSTeK COMPUTER INC. PRIME
/// Z790-A WIFI" -- or "Unknown".
///
/// The board revision used to be appended here as "(5.0)". It is a fact
/// about the PCB, not the name, and in the one-line Summary strip it read as
/// part of the model. It has its own System Info row now.
pub fn get_motherboard_display() -> String {
    let Some(board) = first("Win32_BaseBoard") else {
        eprintln!("Error retrieving motherboard display name: list index out of range");
        return "Unknown".into();
    };
    let parts: Vec<String> = ["Manufacturer", "Product"]
        .iter()
        .map(|field| stripped(&board, field))
        .filter(|part| !part.is_empty())
        .collect();
    let joined = parts.join(" ");
    let joined = py_strip(&joined);
    if joined.is_empty() { "Unknown".into() } else { joined.to_string() }
}

/// The board revision on its own -- "5.0" -- or `None` when firmware has
/// none. Lifted out of the display name, where it read as part of it.
pub fn get_motherboard_version() -> Option<String> {
    MOTHERBOARD_VERSION.get_or_init(|| {
        let version = baseboard("Version");
        if version.is_empty() { None } else { Some(version) }
    })
}

/// One stripped Win32_BaseBoard string, or "" when it is unavailable.
fn baseboard(attribute: &str) -> String {
    match first("Win32_BaseBoard") {
        Some(board) => stripped(&board, attribute),
        None => {
            eprintln!("Error retrieving motherboard {attribute}: list index out of range");
            String::new()
        }
    }
}

/// The date half of a WMI datetime as YYYY-MM-DD, or `None`.
///
/// WMI datetime is yyyymmddHHMMSS.ffffff+UUU; only the date is meaningful for
/// a firmware build or a driver release.
pub fn wmi_date(raw: Option<&str>) -> Option<String> {
    let raw: Vec<char> = raw.unwrap_or("").chars().collect();
    if raw.len() < 8 || !raw[..8].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let part = |range: std::ops::Range<usize>| raw[range].iter().collect::<String>();
    Some(format!("{}-{}-{}", part(0..4), part(4..6), part(6..8)))
}

/// Who made the board, as SMBIOS states it.
pub fn get_board_manufacturer() -> String {
    let value = baseboard("Manufacturer");
    if value.is_empty() { "Unknown".into() } else { value }
}

/// The board product name; revision is displayed in its own row.
pub fn get_board_model() -> String {
    let value = baseboard("Product");
    if value.is_empty() { "Unknown".into() } else { value }
}

// The card, in the order CPU-Z's Graphics tab reads it: what board it is, what
// silicon is on it, then the frame buffer attached to that silicon. Names are
// prefixed where the Processor section already owns the word.
pub const GPU_ROWS: &[(&str, &str)] = &[
    ("Board Manufacturer", "board_manufacturer"),
    ("GPU Code Name", "code_name"),
    ("GPU Revision", "revision"),
    ("Cores", "cores"),
    ("GPU Technology", "technology"),
    ("Memory Size", "memory_size"),
    ("Memory Type", "memory_type"),
    ("Memory Vendor", "memory_vendor"),
    ("Bus Width", "bus_width"),
    ("Resizable BAR", "resizable_bar"),
    ("Driver Version", "driver_version"),
];
// Shown on LGA1700 DDR5 as well: the link the card and slot settled on, and
// the card's own firmware, between Resizable BAR and the driver.
pub const DDR5_GPU_ROWS: &[(&str, &str)] = &[("PCIe Link", "pcie_link"), ("VBIOS Version", "vbios_version")];

/// The installed display adapters, skipping Windows' fallback driver.
///
/// The Basic Display Adapter is what Windows installs before the vendor
/// driver; it is not the card in the machine.
fn real_display_adapters() -> Vec<WmiObject> {
    static_class("Win32_VideoController")
        .iter()
        .filter(|adapter| {
            let name = stripped(adapter, "Name");
            !name.is_empty() && !name.contains("Basic Display")
        })
        .cloned()
        .collect()
}

/// One value from the graphics card, or `None` if it did not answer.
///
/// The adapter ids come from the WMI class this module has already queried
/// and cached, so the card module needs no connection of its own.
pub fn gpu_field(field: &str) -> Option<String> {
    let ids: Vec<String> = real_display_adapters().iter().map(|adapter| adapter.str_or_empty("PNPDeviceID")).collect();
    crate::gpu::nvidia_gpu::read_gpu(false, Some(&ids)).get(field).cloned()
}

pub fn get_bios_version() -> String {
    let Some(bios) = first("Win32_BIOS") else {
        eprintln!("Error retrieving BIOS version: list index out of range");
        return "Unknown".into();
    };
    let primary = bios.get("SMBIOSBIOSVersion").filter(|v| truthy(v)).and_then(WmiValue::to_text);
    let version = primary
        .or_else(|| bios.get("Version").filter(|v| truthy(v)).and_then(WmiValue::to_text))
        .unwrap_or_default();
    let version = py_strip(&version);
    if version.is_empty() { "Unknown".into() } else { version.to_string() }
}

/// Return the actual microcode revision, not the packed registry QWORD.
///
/// Windows commonly stores the revision in the upper 32 bits of the Update
/// Revision value. For example 0x13300000000 means revision 0x133.
pub fn get_microcode() -> String {
    let key = match system_identity::registry::RegKey::open_local_machine(
        system_identity::MICROCODE_REGISTRY_PATH,
    ) {
        Ok(key) => key,
        Err(error) => {
            eprintln!("Error retrieving microcode revision: {error}");
            return "Unknown".into();
        }
    };
    match key.query_value("Update Revision") {
        Ok(value) => system_identity::decode_microcode(&value).unwrap_or_else(|| "Unknown".into()),
        Err(error) => {
            eprintln!("Error retrieving microcode revision: {error}");
            "Unknown".into()
        }
    }
}

pub fn get_ram_manufacturer() -> String {
    let mut manufacturers: Vec<String> = Vec::new();
    for memory in static_class("Win32_PhysicalMemory").iter() {
        let name = stripped(memory, "Manufacturer");
        if !name.is_empty() && !manufacturers.contains(&name) {
            manufacturers.push(name);
        }
    }
    if manufacturers.is_empty() { "Unknown".into() } else { manufacturers.join(", ") }
}

/// The first number in a reading -- "8000.0 MHz" -> 8000.0 -- or `None`.
pub fn parse_first_number(value: Option<&str>) -> Option<f64> {
    let text = value?.replace(',', "");
    let mut number = String::new();
    let mut started = false;
    for ch in text.chars() {
        if ch.is_ascii_digit() || ch == '.' || ch == '-' {
            number.push(ch);
            started = true;
        } else if started {
            break;
        }
    }
    if number.is_empty() {
        return None;
    }
    py_float(&number)
}

/// Python's `float(text)` for the digits, dots and minus signs this sees.
fn py_float(text: &str) -> Option<f64> {
    text.parse::<f64>().ok()
}

// --- System Info identity and per-module rows.

pub const PLATFORM_LABELS: &[(Platform, &str)] = &[
    (Platform::Lga1700Ddr4, "LGA1700 (DDR4)"),
    (Platform::Lga1700Ddr5, "LGA1700 (DDR5)"),
    (Platform::Lga1851, "LGA1851 (DDR5)"),
];

pub const WINDOWS_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

/// The version key's values, read in one open.
///
/// Cached, and read together: these cannot change while the machine runs,
/// and the row that wants them is re-read on every Advanced window tick.
fn windows_version_values() -> std::collections::BTreeMap<&'static str, RegValue> {
    WINDOWS_VERSION_VALUES.get_or_init(system_identity::windows_version_values)
}

/// One value from Windows' own version key, or `None`.
fn windows_version_value(name: &str) -> Option<RegValue> {
    windows_version_values().get(name).cloned()
}

/// The UBR: the fourth part of the build number.
///
/// WMI stops at the build, so 22631 is as much as Win32_OperatingSystem will
/// say. The revision that follows it is what moves with each cumulative
/// update, and it lives in the registry.
pub fn get_windows_update_revision() -> Option<i64> {
    update_revision_from(windows_version_value("UBR"))
}

/// Python's `int(value)` over what `winreg` returned for UBR.
pub(super) fn update_revision_from(value: Option<RegValue>) -> Option<i64> {
    match value? {
        RegValue::Int(value) => i64::try_from(value).ok(),
        RegValue::Text(text) => py_int(&text, 10),
        // int() takes bytes too: ASCII digits, with only ASCII whitespace
        // around them.
        RegValue::Bytes(bytes) => {
            let text = std::str::from_utf8(&bytes).ok().filter(|text| text.is_ascii())?;
            let text = text.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'));
            // py_int would also strip \x1c-\x1f, which int() of a str does
            // and int() of bytes does not.
            let separator = |c: char| ('\x1c'..='\x1f').contains(&c);
            if text.starts_with(separator) || text.ends_with(separator) {
                return None;
            }
            py_int(text, 10)
        }
        _ => None,
    }
}

/// The feature update -- 23H2, 24H2 -- as winver names it.
///
/// DisplayVersion is the one to read. ReleaseId looks like it would do and
/// does not: Microsoft froze it at "2009" when the naming changed, so on this
/// bench it reads 2009 beside a DisplayVersion of 23H2. It is consulted only
/// where DisplayVersion does not exist, which is the era when it was still
/// the accurate one.
pub fn get_windows_display_version() -> Option<String> {
    display_version_from(windows_version_value("DisplayVersion"), || windows_version_value("ReleaseId"))
}

/// The display version from DisplayVersion, or ReleaseId where DisplayVersion
/// is None to `winreg` -- absent, or an empty value of a type it returns as
/// bytes, which it gives as None too.
pub(super) fn display_version_from(
    display_version: Option<RegValue>,
    release_id: impl FnOnce() -> Option<RegValue>,
) -> Option<String> {
    let version = match display_version {
        None | Some(RegValue::None) => release_id(),
        found => found,
    };
    let text = match version {
        Some(value) if value.is_truthy() => value.py_str(),
        _ => String::new(),
    };
    let text = py_strip(&text);
    if text.is_empty() { None } else { Some(text.to_string()) }
}

/// Windows edition, architecture and full build, as CPU-Z states it.
///
/// Its About tab reads "Microsoft Windows 11  Professional (x64) Build
/// 22631.6199", which is the same facts WMI carries plus the update revision
/// -- so the parts are assembled here rather than the caption being printed
/// as-is. "Pro" is spelled the way the edition is spelled: WMI abbreviates it
/// in the caption while the edition itself is Professional.
pub fn get_os_name() -> Option<String> {
    OS_NAME.get_or_init(|| {
        let system = first("Win32_OperatingSystem")?;
        let mut caption = stripped(&system, "Caption");
        if caption.is_empty() {
            return None;
        }
        if !caption.starts_with("Microsoft ") {
            caption = format!("Microsoft {caption}");
        }
        for (short, full) in [("Pro", "Professional"), ("Ent", "Enterprise")] {
            if caption.ends_with(&format!(" {short}")) {
                caption = format!("{}{full}", &caption[..caption.len() - short.len()]);
            }
        }
        let architecture = stripped(&system, "OSArchitecture");
        if !architecture.is_empty() {
            // WMI says "64-bit"; CPU-Z, and everyone else, says x64.
            caption += if architecture.contains("64") { " (x64)" } else { " (x86)" };
        }
        // winver's ordering: the feature update, then the build it produced.
        if let Some(display_version) = get_windows_display_version() {
            caption += &format!(" {display_version}");
        }
        let build = stripped(&system, "BuildNumber");
        if !build.is_empty() {
            let revision = get_windows_update_revision();
            caption += &format!(" Build {build}");
            if let Some(revision) = revision {
                caption += &format!(".{revision}");
            }
        }
        Some(caption)
    })
}

static OS_ARCHITECTURE_SPLIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\((?:x64|x86)\)\s+").expect("valid pattern"));

/// Split the CPU-Z-style OS reading after its architecture.
fn os_name_and_version() -> (Option<String>, Option<String>) {
    let Some(combined) = get_os_name().filter(|name| !name.is_empty()) else {
        return (None, None);
    };
    let Some(found) = OS_ARCHITECTURE_SPLIT.find(&combined) else {
        return (Some(combined), None);
    };
    let edition = py_strip(&combined[..found.end()]).to_string();
    let version = py_strip(&combined[found.end()..]).to_string();
    (Some(edition), if version.is_empty() { None } else { Some(version) })
}

/// Windows product name and architecture, without release/build.
pub fn get_os_edition() -> Option<String> {
    os_name_and_version().0
}

/// Windows feature update and full build, as a separate row.
pub fn get_os_version() -> Option<String> {
    os_name_and_version().1
}

/// Release date of the installed display driver.
///
/// The version alone does not say how old a driver is. Same WMI datetime
/// shape as the BIOS release date, and rendered the same way.
pub fn get_gpu_driver_date() -> Option<String> {
    real_display_adapters()
        .iter()
        .find_map(|adapter| wmi_date(adapter.text("DriverDate").as_deref()))
}

/// Every real display adapter.
///
/// The Microsoft Basic Display driver is skipped: Windows installs it before
/// the vendor driver and it is not the card in the machine.
pub fn get_gpu_name() -> Option<String> {
    let mut names: Vec<String> = Vec::new();
    for adapter in static_class("Win32_VideoController").iter() {
        let name = stripped(adapter, "Name");
        if !name.is_empty() && !name.contains("Basic Display") && !names.contains(&name) {
            names.push(name);
        }
    }
    if names.is_empty() { None } else { Some(names.join(" / ")) }
}

/// Release date of the installed BIOS.
///
/// The version string alone does not say how old a build is, and on this board
/// the vendor reuses short version numbers across releases.
pub fn get_bios_date() -> Option<String> {
    let bios = first("Win32_BIOS")?;
    wmi_date(bios.text("ReleaseDate").as_deref())
}

/// Populated sockets against the board's total, e.g. "2 of 4".
///
/// Two modules in a four-slot board behave differently from four, so the
/// total matters as much as the count.
///
/// The total is the number of sockets firmware enumerates, not the array size
/// it declares. Those disagree on this bench: a two-slot board reports
/// MemoryDevices 4 in the memory-array record while listing exactly two
/// sockets, Controller0-ChannelA-DIMM0 and ChannelB-DIMM0, with no DIMM1 on
/// either channel. The declared size is a property of the reference design
/// the firmware came from; the socket list is what is actually there, and it
/// is the one that names each slot.
///
/// The array size is still the fallback, for firmware that omits its empty
/// sockets rather than listing them at zero capacity. That case cannot be
/// told from a full board by counting alone, so the larger of the two wins:
/// over-reporting slots costs a wrong denominator, while under-reporting
/// would claim a board is full when it is not.
pub fn get_slots_used() -> Option<String> {
    let run = || -> Result<Option<String>, String> {
        let sockets = static_class("Win32_PhysicalMemory");
        let mut used = 0;
        for socket in sockets.iter() {
            if int_or_zero(socket, "Capacity")? > 0 {
                used += 1;
            }
        }
        let mut total = sockets.len() as i64;
        let arrays = static_class("Win32_PhysicalMemoryArray");
        if let Some(array) = arrays.first() {
            let declared = int_or_zero(array, "MemoryDevices")?;
            if declared > total {
                // Only when the socket list is missing entries, which is what
                // a declared size larger than the list it describes means --
                // unless the list is complete, in which case see above.
                total = if total != 0 { total } else { declared };
            }
        }
        if total == 0 {
            return Ok(None);
        }
        Ok(Some(format!("{used} of {total}")))
    };
    run().unwrap_or_else(|error| {
        eprintln!("Error retrieving slot usage: {error}");
        None
    })
}

/// Socket and memory generation, named by the classification that chose
/// this backend rather than by a second guess at the same facts.
///
/// From the platform the table was built for, for that reason, and for a
/// second one: this row is read on every refresh, and detecting the platform
/// opens a WMI connection and runs three queries -- 1.08 seconds on the bench,
/// measured. Asking here put that second into each pass over the table.
pub fn get_platform_name(platform: Platform) -> Option<String> {
    PLATFORM_LABELS.iter().find(|(p, _)| *p == platform).map(|(_, label)| label.to_string())
}

// Fields the module's own SPD answers, and what stands in when the bus cannot
// be reached -- no driver or no admin access. A None fallback means nothing
// else on the machine carries the field, so the row reports nothing:
// SMBIOS has no build date at all, and the maker/die lookup is keyed on the
// part number and so cannot know an unlisted kit.
//
// The serial is the exception among those three: firmware does read it off the
// module at POST and puts it in Type 17, so it comes back through the same
// placeholder filter as the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpdFallback {
    /// A firmware string, unless firmware left it as a placeholder.
    NamedOrNothing,
    /// The DRAM maker or die inferred from the module's part number.
    IcLookup,
}

pub const SPD_IDENTITY_FIELDS: &[(&str, Option<SpdFallback>)] = &[
    ("module_manufacturer", Some(SpdFallback::NamedOrNothing)),
    ("part_number", Some(SpdFallback::NamedOrNothing)),
    ("serial_number", Some(SpdFallback::NamedOrNothing)),
    ("manufacture_date", None),
    ("dram_manufacturer", Some(SpdFallback::IcLookup)),
    ("dram_die", Some(SpdFallback::IcLookup)),
];

// What the DDR4 SPD is asked for. The DRAM manufacturer, build date, and serial
// come directly from the module. The module vendor stays on the SMBIOS path
// because some valid DDR4 vendor IDs are not named by our local JEP106 table.
//
// The rest are deliberately left on the path that already answers them. The
// DDR4 block does carry a DRAM maker and a stepping, but the stepping is the
// byte the die name would come from and it reads 0x00 on this kit -- taking
// SPD first there would turn a "B-die" that the part-number table gets right
// into a "0x00" that is technically what the module said and no use to anyone.
pub const DDR4_SPD_FIELDS: &[&str] = &["serial_number", "manufacture_date", "dram_manufacturer"];

fn named_or_nothing(module: &crate::memory::dimm_inventory::DimmModule, field: &str) -> Option<String> {
    let value = module.get(field).unwrap_or_default();
    let value = py_strip(&value);
    if value.is_empty() || value.to_lowercase() == "unknown" { None } else { Some(value.to_string()) }
}

fn ic_lookup(module: &crate::memory::dimm_inventory::DimmModule, field: &str) -> Option<String> {
    let (maker, die) = crate::memory::dimm_inventory::split_ic(Some(&module.ic));
    Some(if field == "dram_manufacturer" { maker } else { die })
}

/// What the modules themselves say about one field, or `None`.
///
/// Two readers are picked by generation because the SPD layouts and page
/// selection protocols differ. See ddr4_spd and ddr5_spd.
fn spd_identity(field: &str) -> Option<String> {
    use crate::memory::dimm_inventory::{EM_DASH, shared_value};

    let modules = if detect_ddr_generation() == DdrGeneration::Ddr4 {
        if !DDR4_SPD_FIELDS.contains(&field) {
            return None;
        }
        crate::memory::ddr4_spd::read_identity(None, false)
    } else {
        crate::memory::ddr5_spd::read_identity(None, false, None)
    };
    if modules.is_empty() {
        return None;
    }
    let value = shared_value(&modules, |entry| entry.get(field));
    if value == EM_DASH { None } else { Some(value) }
}

/// A per-DIMM System Info value for the whole installed set.
///
/// Size and rank come from SMBIOS Type 17, which is firmware reporting what it
/// read from the modules at boot.
///
/// The DRAM maker, build date and serial come from the module's own SPD, read
/// over the PCH SMBus. Die naming uses the exact part-number table on DDR4
/// because a stepping byte alone does not identify every die. If the bus is
/// unreachable, the maker and die fall back to that table and the remaining
/// values use SMBIOS where available.
///
/// Reported for the set rather than one slot, since the controller settings
/// above these rows are equally set-wide. A mixed kit prints each distinct
/// value instead of hiding the mismatch.
pub fn dimm_field(field: &str) -> Option<String> {
    use crate::memory::dimm_inventory::{EM_DASH, rank_numeric, read_modules, shared_value};

    if let Some((_, fallback)) = SPD_IDENTITY_FIELDS.iter().find(|(name, _)| *name == field) {
        // The module first, firmware second. SMBIOS carries no date at all,
        // reports a serial only when firmware found one, and where it does
        // carry a name it is the string a board vendor typed: "V-Color
        // Technology Inc" against the "V-Color Technology" the module itself
        // reports.
        if let Some(value) = spd_identity(field) {
            return Some(value);
        }
        let fallback = (*fallback)?;
        let value = shared_value(&read_modules(false), |module| match fallback {
            SpdFallback::NamedOrNothing => named_or_nothing(module, field),
            SpdFallback::IcLookup => ic_lookup(module, field),
        });
        return if value == EM_DASH { None } else { Some(value) };
    }

    let modules = read_modules(false);
    let value = match field {
        "size" => shared_value(&modules, |module| {
            Some(if module.capacity_gb != 0 { format!("{} GB", module.capacity_gb) } else { EM_DASH.to_string() })
        }),
        "rank" => shared_value(&modules, |module| Some(rank_numeric(module.rank_count))),
        _ => return None,
    };
    // Hand the em dash back as "no value" so every unavailable row in the
    // table renders the same way.
    if value == EM_DASH { None } else { Some(value) }
}

// --- Channel layout.

// DDR5 splits every memory channel into two independent 32-bit sub-channels
// (JESD79-5), and it is the sub-channels the controller schedules against.
// This bench has DIMMs in channels A and B and trains four separate round-trip
// latencies for them -- MC0/MC1 x CHA/CHB read 70, 65, 71 and 65, with rank 1
// left at the unpopulated 25 on all four. Four sub-channels, one rank each.
//
// What this row counts is decided by the platform, because the tools it has to
// agree with disagree with each other.
//
// Two DDR5 boards, two modules in each, and the same two reference tools:
//
//   Z790 APEX    (LGA1700 DDR5)   ASRock Timing Configurator, HWiNFO: Quad / 4
//   Z890 TACHYON (LGA1851)        the same two tools:                 Dual / 2
//
// Both numbers are true of both boards. Two DIMM channels, four sub-channels,
// and the RTL block a tab away trains four round-trip latencies either way.
// What differs is the convention each platform's tools follow, and a viewer
// exists to agree with them -- so the factor is keyed on the platform, not on
// the memory generation.
//
// This was DDR5-wide in both directions before, and both were wrong for one of
// these boards: doubling everywhere made the Tachyon say Quad against a tool
// showing Dual, and then doubling nowhere made the APEX say Dual against a
// tool showing Quad. One board cannot settle the other.
pub const SUBCHANNEL_COUNTED_PLATFORMS: &[Platform] = &[Platform::Lga1700Ddr5];
pub const DDR5_SUBCHANNELS_PER_CHANNEL: usize = 2;

/// How many channels this platform's reference tools show for that many.
///
/// Pure, so the convention can be tested for a platform without standing in
/// front of one.
pub fn reported_channel_count(populated_channels: usize, platform: Option<Platform>) -> usize {
    if populated_channels == 0 {
        return 0;
    }
    if platform.is_some_and(|p| SUBCHANNEL_COUNTED_PLATFORMS.contains(&p)) {
        return populated_channels * DDR5_SUBCHANNELS_PER_CHANNEL;
    }
    populated_channels
}

pub const CHANNEL_LAYOUT_NAMES: &[(usize, &str)] = &[
    (1, "Single Channel"),
    (2, "Dual Channel"),
    (3, "Triple Channel"),
    (4, "Quad Channel"),
    (6, "Six Channel"),
    (8, "Eight Channel"),
];

/// Name the layout for a count of populated channels, as `platform` counts
/// them. Pure, so it can be tested without a machine to read.
pub fn channel_layout_name(populated_channels: usize, platform: Option<Platform>) -> Option<String> {
    let count = reported_channel_count(populated_channels, platform);
    if count == 0 {
        return None;
    }
    Some(
        CHANNEL_LAYOUT_NAMES
            .iter()
            .find(|(n, _)| *n == count)
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| format!("{count} Channels")),
    )
}

/// How many channels the controller runs, named as the reference tools do.
///
/// Counts the channels the installed modules populate, then names them the
/// way this platform's reference tools do. See SUBCHANNEL_COUNTED_PLATFORMS
/// for why that is not one rule for all of DDR5.
///
/// Falls back to the slot-tag reading below when the module inventory is
/// unavailable. Both paths now answer the same question, which they did not
/// when this one doubled and that one did not.
pub fn detect_dual_channel_memory(platform: Platform) -> Option<String> {
    let mut channels: Vec<String> = Vec::new();
    for module in crate::memory::dimm_inventory::read_modules(false) {
        if let Some(channel) = module.channel.filter(|c| !c.is_empty())
            && !channels.contains(&channel) {
                channels.push(channel);
            }
    }
    if let Some(named) = channel_layout_name(channels.len(), Some(platform)) {
        return Some(named);
    }
    channel_layout_from_slot_tags(platform)
}

// The board's own socket name carries the channel; a slot count does not.
// "DIMMA2", "Controller0-DIMMB1" and "ChannelC-DIMM0" all name one, and
// counting the distinct letters asks the same question the inventory path
// asks, of the same SMBIOS records.
static LOCATOR_CHANNEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:CHANNEL|CH|DIMM)[ _-]?([A-H])").expect("valid pattern"));

/// Whether Python's `[A-Z]` under `re.I` matches `c`: an ASCII letter, or
/// one of the four characters whose simple case mapping is one -- the long s
/// and the Kelvin sign, and the Turkish dotless and dotted I.
fn is_ascii_letter_ignoring_case(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, '\u{17F}' | '\u{212A}' | '\u{131}' | '\u{130}')
}

/// The first match of the locator pattern, with Python's `(?![A-Z])`
/// lookahead (case-insensitively) applied by hand, since `regex` has none.
pub(super) fn locator_channel(locator: &str) -> Option<char> {
    // Python's re.I also lets the dotless and dotted I stand for the I of
    // "DIMM"; Unicode case folding, which `regex` uses, does not.
    let normalised;
    let locator = if locator.contains(['\u{131}', '\u{130}']) {
        normalised = locator.replace(['\u{131}', '\u{130}'], "I");
        normalised.as_str()
    } else {
        locator
    };
    let mut start = 0;
    while start <= locator.len() {
        let captures = LOCATOR_CHANNEL.captures_at(locator, start)?;
        let letter = captures.get(1).expect("one group");
        let next = locator[letter.end()..].chars().next();
        if !next.is_some_and(is_ascii_letter_ignoring_case) {
            return letter.as_str().chars().next().map(|c| c.to_ascii_uppercase());
        }
        // No match here once the lookahead fails; try from the next position.
        let whole = captures.get(0).expect("whole match");
        start = whole.start() + whole.as_str().chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// The distinct channel letters the board names for its populated slots.
///
/// Capacity is checked rather than assumed. Most firmware lists only the
/// sockets that hold a module, but some emits an entry for every socket with
/// a capacity of zero, and counting those would report a channel the board
/// has nothing in. get_slots_used draws the same distinction.
pub fn channels_from_locators(modules: &[WmiObject]) -> Vec<char> {
    let mut letters: Vec<char> = Vec::new();
    for module in modules {
        // Only a socket that says it is empty is skipped. A missing or
        // unreadable capacity is not evidence of an empty slot, and treating
        // it as one would drop a channel the board really has.
        if let Some(capacity) = module.get("Capacity")
            && !matches!(capacity, WmiValue::Null) && capacity.to_i64() == Some(0) {
                continue;
            }
        if let Some(letter) = locator_channel(&module.str_or_empty("DeviceLocator"))
            && !letters.contains(&letter) {
                letters.push(letter);
            }
    }
    letters
}

/// Channel layout from SMBIOS alone, when the module inventory is absent.
///
/// Reads the socket names first, because they state the channel. Counting
/// slots cannot: the reading below only ever separated one populated group
/// from two, so every four-slot board came back Dual -- correct for the
/// two-DIMM-per-channel consumer boards this tool runs on, and wrong for a
/// genuinely four-channel one, which it had no way to name at all.
///
/// A board whose sockets say nothing is now reported as unread rather than
/// called Dual. Two slots stays a real answer either way: two populated
/// sockets cannot be more than two channels.
pub(super) fn channel_layout_from_slot_tags(platform: Platform) -> Option<String> {
    let modules = static_class("Win32_PhysicalMemory");
    if let Some(named) = channel_layout_name(channels_from_locators(&modules).len(), Some(platform)) {
        return Some(named);
    }
    let memory_arrays = static_class("Win32_PhysicalMemoryArray");
    let Some(array) = memory_arrays.first() else {
        return Some("No memory array detected".into());
    };
    let Some(num_slots) = array.get("MemoryDevices").cloned() else {
        eprintln!("Error detecting memory layout: 'MemoryDevices'");
        return None;
    };
    let mut used_slots: Vec<Option<String>> = Vec::new();
    for module in modules.iter() {
        let tag = module.text("Tag");
        if !used_slots.contains(&tag) {
            used_slots.push(tag);
        }
    }
    if used_slots.len() <= 1 {
        return Some("Single Channel".into());
    }
    // Python's `num_slots == 2`: a number equal to two, not a string.
    let two = match num_slots {
        WmiValue::Int(value) => value == 2,
        WmiValue::UInt(value) => value == 2,
        WmiValue::Float(value) => value == 2.0,
        _ => false,
    };
    if two {
        return Some("Dual Channel".into());
    }
    Some(format!(
        "{} DIMM slots detected - Unknown Channel Layout",
        num_slots.to_text().unwrap_or_else(|| "None".into())
    ))
}
