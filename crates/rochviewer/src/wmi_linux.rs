//! What WMI answers on Windows, from where Linux keeps it.
//!
//! Nothing above this module knows the difference. It builds the same objects
//! the same classes return, with the same property names and the same units, so
//! every decoder, table and row reads them unchanged:
//!
//! | class | source |
//! | --- | --- |
//! | `Win32_Processor` | CPUID, `/proc/cpuinfo`, `/sys/devices/system/cpu`, SMBIOS type 4 |
//! | `Win32_BaseBoard`, `Win32_BIOS` | `/sys/class/dmi/id` |
//! | `Win32_ComputerSystem` | `/sys/class/dmi/id`, `/proc/meminfo` |
//! | `Win32_OperatingSystem` | `/etc/os-release`, `uname`, `/proc/stat` |
//! | `Win32_PhysicalMemory`, `Win32_PhysicalMemoryArray` | SMBIOS types 17 and 16 |
//! | `Win32_VideoController` | `/sys/bus/pci/devices`, libdrm's and pciutils' name tables |
//!
//! The one catch is that Windows shows SMBIOS to anybody and Linux only to root,
//! so the memory classes go through the hardware backend. Without it there is
//! still one thing to go on: the kernel names the SPD devices it found, which is
//! enough for the generation, and so for the platform.

use std::collections::BTreeMap;
use std::path::Path;

use super::WmiObject;
use crate::hardware::hw;

const DMI_ID: &str = "/sys/class/dmi/id";
const DMI_TABLE: &str = "/sys/firmware/dmi/tables/DMI";
const CPU_ROOT: &str = "/sys/devices/system/cpu";
const PCI_ROOT: &str = "/sys/bus/pci/devices";

/// SMBIOS Type 17 memory types, the two the kernel's SPD drivers imply.
const SMBIOS_DDR4: i64 = 0x1A;
const SMBIOS_DDR5: i64 = 0x22;

pub fn query(class_name: &str) -> Result<Vec<WmiObject>, String> {
    match class_name {
        "Win32_Processor" => Ok(vec![processor()]),
        "Win32_BaseBoard" => Ok(vec![base_board()]),
        "Win32_BIOS" => Ok(vec![bios()]),
        "Win32_ComputerSystem" => Ok(vec![computer_system()]),
        "Win32_OperatingSystem" => Ok(vec![operating_system()]),
        "Win32_PhysicalMemory" => Ok(physical_memory()),
        "Win32_PhysicalMemoryArray" => Ok(physical_memory_array()),
        "Win32_VideoController" => Ok(video_controllers()),
        other => Err(format!("{other} has no source on Linux")),
    }
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn dmi_field(name: &str) -> Option<String> {
    read_trimmed(format!("{DMI_ID}/{name}"))
}

/// A DMI field, through the backend when it is one only root may read.
fn dmi_private_field(name: &str) -> Option<String> {
    dmi_field(name).or_else(|| {
        let bytes = hw().read_firmware_file(&format!("{DMI_ID}/{name}")).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    })
}

fn with_text(object: WmiObject, field: &str, value: Option<String>) -> WmiObject {
    match value {
        Some(value) => object.with(field, value),
        None => object,
    }
}

// --- SMBIOS

/// One SMBIOS structure: its formatted area and its strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmbiosStructure {
    pub kind: u8,
    formatted: Vec<u8>,
    strings: Vec<String>,
}

impl SmbiosStructure {
    pub fn byte(&self, offset: usize) -> Option<u8> {
        self.formatted.get(offset).copied()
    }

    pub fn word(&self, offset: usize) -> Option<u16> {
        let bytes = self.formatted.get(offset..offset + 2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn dword(&self, offset: usize) -> Option<u32> {
        let bytes = self.formatted.get(offset..offset + 4)?;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn qword(&self, offset: usize) -> Option<u64> {
        let bytes = self.formatted.get(offset..offset + 8)?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }

    /// The string whose 1-based index is stored at `offset`.
    pub fn string(&self, offset: usize) -> Option<String> {
        let index = self.byte(offset)? as usize;
        if index == 0 {
            return None;
        }
        self.strings.get(index - 1).cloned()
    }
}

/// Walk a raw SMBIOS table. A truncated structure ends the walk.
pub fn parse_smbios(table: &[u8]) -> Vec<SmbiosStructure> {
    let mut found = Vec::new();
    let mut at = 0usize;
    while at + 4 <= table.len() {
        let kind = table[at];
        let length = table[at + 1] as usize;
        if length < 4 || at + length > table.len() {
            break;
        }
        let formatted = table[at..at + length].to_vec();
        // The strings run from there to a double NUL.
        let mut cursor = at + length;
        let mut strings = Vec::new();
        while let Some(rest) = table.get(cursor..) {
            let Some(end) = rest.iter().position(|b| *b == 0) else {
                cursor = table.len();
                break;
            };
            if end == 0 {
                // An empty string is the terminator, and a structure with no
                // strings at all has a second NUL straight after this one.
                cursor += if strings.is_empty() { 2 } else { 1 };
                break;
            }
            strings.push(String::from_utf8_lossy(&rest[..end]).into_owned());
            cursor += end + 1;
        }
        found.push(SmbiosStructure { kind, formatted, strings });
        if kind == 127 {
            break;
        }
        at = cursor;
    }
    found
}

fn smbios_table() -> Option<Vec<SmbiosStructure>> {
    if cfg!(test) {
        return None;
    }
    hw().read_firmware_file(DMI_TABLE).ok().map(|table| parse_smbios(&table))
}

/// A module's capacity in bytes from its Size and Extended Size fields, or
/// `None` for an empty socket.
pub fn module_capacity(size: u16, extended_mb: Option<u32>) -> Option<u64> {
    match size {
        0 | 0xFFFF => None,
        0x7FFF => Some(((extended_mb? & 0x7FFF_FFFF) as u64) << 20),
        size if size & 0x8000 != 0 => Some(((size & 0x7FFF) as u64) << 10),
        size => Some((size as u64) << 20),
    }
}

/// A speed field, with the extended field that replaces it when it reads 0xFFFF.
fn module_speed(speed: Option<u16>, extended: Option<u32>) -> Option<u64> {
    match speed? {
        0 => None,
        0xFFFF => extended.map(|value| (value & 0x7FFF_FFFF) as u64).filter(|value| *value != 0),
        speed => Some(speed as u64),
    }
}

/// `Win32_PhysicalMemory` from Type 17. Installed modules only, as WMI lists
/// them: a socket with nothing in it has no capacity and is not an object.
pub fn physical_memory_from(structures: &[SmbiosStructure]) -> Vec<WmiObject> {
    let mut modules = Vec::new();
    for (index, device) in structures.iter().filter(|s| s.kind == 17).enumerate() {
        let Some(capacity) = device.word(0x0C).and_then(|size| module_capacity(size, device.dword(0x1C))) else {
            continue;
        };
        let mut module = WmiObject::new()
            .with("Name", "Physical Memory")
            .with("Description", "Physical Memory")
            .with("Tag", format!("Physical Memory {index}"))
            .with("Capacity", capacity.to_string());
        for (field, offset) in [
            ("DeviceLocator", 0x10),
            ("BankLabel", 0x11),
            ("Manufacturer", 0x17),
            ("SerialNumber", 0x18),
            ("AssetTag", 0x19),
            ("PartNumber", 0x1A),
        ] {
            module = with_text(module, field, device.string(offset));
        }
        for (field, offset) in [
            ("TotalWidth", 0x08),
            ("DataWidth", 0x0A),
            ("MinVoltage", 0x22),
            ("MaxVoltage", 0x24),
            ("ConfiguredVoltage", 0x26),
            ("TypeDetail", 0x13),
        ] {
            if let Some(value) = device.word(offset).filter(|value| *value != 0xFFFF) {
                module = module.with(field, value as u64);
            }
        }
        if let Some(form_factor) = device.byte(0x0E) {
            module = module.with("FormFactor", form_factor as u64);
        }
        if let Some(memory_type) = device.byte(0x12) {
            module = module.with("SMBIOSMemoryType", memory_type as u64).with("MemoryType", memory_type as u64);
        }
        // Bits 3:0 are the rank count, which is what WMI's Attributes reports.
        if let Some(attributes) = device.byte(0x1B) {
            module = module.with("Attributes", (attributes & 0x0F) as u64);
        }
        if let Some(speed) = module_speed(device.word(0x15), device.dword(0x54)) {
            module = module.with("Speed", speed);
        }
        if let Some(speed) = module_speed(device.word(0x20), device.dword(0x58)) {
            module = module.with("ConfiguredClockSpeed", speed);
        }
        modules.push(module);
    }
    modules
}

/// `Win32_PhysicalMemoryArray` from Type 16.
pub fn physical_memory_array_from(structures: &[SmbiosStructure]) -> Vec<WmiObject> {
    structures
        .iter()
        .filter(|s| s.kind == 16)
        .enumerate()
        .map(|(index, array)| {
            let mut object = WmiObject::new().with("Tag", format!("Physical Memory Array {index}"));
            if let Some(devices) = array.word(0x0D) {
                object = object.with("MemoryDevices", devices as u64);
            }
            let kilobytes = match array.dword(0x07) {
                Some(0x8000_0000) => array.qword(0x0F).map(|bytes| bytes >> 10),
                other => other.map(u64::from),
            };
            if let Some(kilobytes) = kilobytes {
                object = object.with("MaxCapacity", kilobytes);
            }
            object
        })
        .collect()
}

/// The generation, from the SPD drivers the kernel bound: all there is without
/// the SMBIOS table, and enough to tell the platform.
fn memory_type_from_spd_devices() -> Vec<i64> {
    let Ok(devices) = std::fs::read_dir("/sys/bus/i2c/devices") else {
        return Vec::new();
    };
    devices
        .flatten()
        .filter_map(|entry| match read_trimmed(entry.path().join("name")).as_deref() {
            Some("spd5118") => Some(SMBIOS_DDR5),
            Some("ee1004") => Some(SMBIOS_DDR4),
            _ => None,
        })
        .collect()
}

fn physical_memory() -> Vec<WmiObject> {
    match smbios_table() {
        Some(structures) => physical_memory_from(&structures),
        None => memory_type_from_spd_devices()
            .into_iter()
            .map(|memory_type| WmiObject::new().with("SMBIOSMemoryType", memory_type).with("MemoryType", memory_type))
            .collect(),
    }
}

fn physical_memory_array() -> Vec<WmiObject> {
    smbios_table().map(|structures| physical_memory_array_from(&structures)).unwrap_or_default()
}

// --- Processor

/// The fields of `/proc/cpuinfo`'s first processor.
pub fn cpuinfo_fields(text: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            fields.entry(name.trim().to_string()).or_insert_with(|| value.trim().to_string());
        }
    }
    fields
}

/// CPUID leaf 1 as WMI's `ProcessorId` writes it: EDX, then EAX, in hex.
#[cfg(target_arch = "x86_64")]
fn processor_id() -> Option<String> {
    let leaf = std::arch::x86_64::__cpuid(1);
    Some(format!("{:08X}{:08X}", leaf.edx, leaf.eax))
}

#[cfg(not(target_arch = "x86_64"))]
fn processor_id() -> Option<String> {
    None
}

/// `/sys/devices/system/cpu/online`'s "0-7,16-23" as CPU numbers.
pub fn parse_cpu_list(text: &str) -> Vec<u32> {
    let mut cpus = Vec::new();
    for part in text.trim().split(',').filter(|part| !part.is_empty()) {
        match part.split_once('-') {
            Some((first, last)) => {
                if let (Ok(first), Ok(last)) = (first.trim().parse::<u32>(), last.trim().parse::<u32>()) {
                    cpus.extend(first..=last);
                }
            }
            None => cpus.extend(part.trim().parse::<u32>()),
        }
    }
    cpus
}

pub fn online_cpus() -> Vec<u32> {
    read_trimmed(format!("{CPU_ROOT}/online")).map(|list| parse_cpu_list(&list)).unwrap_or_default()
}

/// Each online physical core's logical processors, one list per core.
pub fn physical_cores() -> Vec<Vec<u32>> {
    let mut cores: Vec<Vec<u32>> = online_cpus()
        .into_iter()
        .filter_map(|cpu| read_trimmed(format!("{CPU_ROOT}/cpu{cpu}/topology/core_cpus_list")))
        .map(|list| parse_cpu_list(&list))
        .collect();
    cores.sort();
    cores.dedup();
    cores
}

/// `36864K` as kilobytes.
fn cache_kilobytes(cpu_relative: &str) -> Option<u64> {
    let text = read_trimmed(format!("{CPU_ROOT}/cpu0/cache/{cpu_relative}/size"))?;
    let digits = text.strip_suffix('K').unwrap_or(&text);
    digits.parse().ok()
}

fn processor() -> WmiObject {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let fields = cpuinfo_fields(&cpuinfo);
    let name = fields.get("model name").cloned();
    let mut cpu = WmiObject::new().with("Caption", name.clone().unwrap_or_default());
    cpu = with_text(cpu, "Name", name);
    cpu = with_text(cpu, "Manufacturer", fields.get("vendor_id").cloned());
    cpu = with_text(cpu, "ProcessorId", processor_id());
    let cores = physical_cores().len();
    let threads = online_cpus().len();
    if cores > 0 {
        cpu = cpu.with("NumberOfCores", cores as u64).with("NumberOfEnabledCore", cores as u64);
    }
    if threads > 0 {
        cpu = cpu.with("NumberOfLogicalProcessors", threads as u64).with("ThreadCount", threads as u64);
    }
    if let Some(megahertz) = read_trimmed(format!("{CPU_ROOT}/cpu0/cpufreq/cpuinfo_max_freq"))
        .and_then(|khz| khz.parse::<u64>().ok())
    {
        cpu = cpu.with("MaxClockSpeed", megahertz / 1000);
    }
    if let Some(kilobytes) = cache_kilobytes("index3") {
        cpu = cpu.with("L3CacheSize", kilobytes);
    }
    if let Some(kilobytes) = cache_kilobytes("index2") {
        cpu = cpu.with("L2CacheSize", kilobytes);
    }
    // The socket and the external clock are SMBIOS's alone.
    if let Some(socket) = smbios_table().and_then(|structures| {
        structures.into_iter().find(|structure| structure.kind == 4)
    }) {
        cpu = with_text(cpu, "SocketDesignation", socket.string(0x04));
        if let Some(clock) = socket.word(0x12).filter(|clock| *clock != 0) {
            cpu = cpu.with("ExtClock", clock as u64);
        }
    }
    cpu
}

// --- The board and the BIOS

fn base_board() -> WmiObject {
    let mut board = WmiObject::new();
    for (field, source) in [("Manufacturer", "board_vendor"), ("Product", "board_name"), ("Version", "board_version")] {
        board = with_text(board, field, dmi_field(source));
    }
    with_text(board, "SerialNumber", dmi_private_field("board_serial"))
}

/// `04/29/2026` as WMI's datetime, which is how every caller expects a date.
pub fn cim_date(dmi_date: &str) -> Option<String> {
    let mut parts = dmi_date.trim().split('/');
    let (month, day, year) = (parts.next()?, parts.next()?, parts.next()?);
    let (month, day, year): (u32, u32, u32) = (month.parse().ok()?, day.parse().ok()?, year.parse().ok()?);
    Some(format!("{year:04}{month:02}{day:02}000000.000000+000"))
}

fn bios() -> WmiObject {
    let mut bios = WmiObject::new();
    let version = dmi_field("bios_version");
    bios = with_text(bios, "SMBIOSBIOSVersion", version.clone());
    bios = with_text(bios, "Version", version);
    bios = with_text(bios, "Manufacturer", dmi_field("bios_vendor"));
    with_text(bios, "ReleaseDate", dmi_field("bios_date").and_then(|date| cim_date(&date)))
}

/// `MemTotal:  65532704 kB` as bytes.
pub fn mem_total_bytes(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kilobytes * 1024)
}

fn computer_system() -> WmiObject {
    let mut system = WmiObject::new();
    system = with_text(system, "Manufacturer", dmi_field("sys_vendor"));
    system = with_text(system, "Model", dmi_field("product_name"));
    if let Some(bytes) = std::fs::read_to_string("/proc/meminfo").ok().as_deref().and_then(mem_total_bytes) {
        system = system.with("TotalPhysicalMemory", bytes.to_string());
    }
    system
}

// --- The operating system

/// One value from `/etc/os-release`, unquoted.
pub fn os_release_field(text: &str, name: &str) -> Option<String> {
    let line = text.lines().find(|line| line.split_once('=').is_some_and(|(key, _)| key == name))?;
    let value = line.split_once('=')?.1.trim();
    let value = value.trim_matches(|c| c == '"' || c == '\'');
    (!value.is_empty()).then(|| value.to_string())
}

/// The distribution's name with its version, once.
pub fn os_caption(os_release: &str) -> String {
    let name = os_release_field(os_release, "PRETTY_NAME")
        .or_else(|| os_release_field(os_release, "NAME"))
        .unwrap_or_else(|| "Linux".to_string());
    match os_release_field(os_release, "VERSION_ID") {
        Some(version) if !name.contains(&version) => format!("{name} {version}"),
        _ => name,
    }
}

fn kernel_release() -> Option<String> {
    read_trimmed("/proc/sys/kernel/osrelease")
}

/// Seconds since the epoch when the machine booted.
fn boot_time() -> Option<i64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    stat.lines().find_map(|line| line.strip_prefix("btime "))?.trim().parse().ok()
}

/// An epoch time as a CIM datetime in local time.
fn cim_local(seconds: i64) -> Option<String> {
    // SAFETY: an all-zero `struct tm` is valid, and localtime_r only writes it.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let seconds = seconds as libc::time_t;
        if libc::localtime_r(&seconds, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}.000000+000",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    ))
}

fn operating_system() -> WmiObject {
    let os_release = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap_or_default();
    let architecture = if cfg!(target_pointer_width = "64") { "64-bit" } else { "32-bit" };
    let mut system = WmiObject::new().with("Caption", os_caption(&os_release)).with("OSArchitecture", architecture);
    let release = kernel_release();
    system = with_text(system, "Version", release.clone());
    system = with_text(system, "BuildNumber", release);
    with_text(system, "LastBootUpTime", boot_time().and_then(cim_local))
}

// --- Display adapters

/// The marketing name libdrm's table gives a device and revision.
///
/// The right table to ask first where it has the card, because one device id
/// covers a family and the revision is what separates its cuts: the Z890 bench's
/// RX 7900 XT is 0x744C revision 0xCC, which `pci.ids` can only call all four
/// things 0x744C sells as, and which its subsystem line names as a PULSE XTX.
pub fn amdgpu_ids_name(table: &str, device: u16, revision: u8) -> Option<String> {
    table.lines().find_map(|line| {
        let mut fields = line.split(',');
        let id = u16::from_str_radix(fields.next()?.trim(), 16).ok()?;
        let rev = u8::from_str_radix(fields.next()?.trim(), 16).ok()?;
        let name = fields.next()?.trim();
        (id == device && rev == revision && !name.is_empty()).then(|| name.to_string())
    })
}

/// What `pci.ids` calls a device: its subsystem's line if it has one, otherwise
/// the part in brackets, which is the product rather than the die.
pub fn pci_ids_name(table: &str, vendor: u16, device: u16, sub_vendor: u16, sub_device: u16) -> Option<String> {
    let mut in_vendor = false;
    let mut device_name: Option<String> = None;
    for line in table.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if !line.starts_with('\t') {
            if in_vendor {
                break;
            }
            let id = line.split_whitespace().next().and_then(|id| u16::from_str_radix(id, 16).ok());
            in_vendor = id == Some(vendor) && line.len() >= 4 && !line.starts_with('C');
            continue;
        }
        if !in_vendor {
            continue;
        }
        if let Some(sub) = line.strip_prefix("\t\t") {
            if device_name.is_some() {
                let mut parts = sub.split_whitespace();
                let (v, d) = (parts.next(), parts.next());
                let matches = v.and_then(|v| u16::from_str_radix(v, 16).ok()) == Some(sub_vendor)
                    && d.and_then(|d| u16::from_str_radix(d, 16).ok()) == Some(sub_device);
                if matches {
                    let name = sub.splitn(3, char::is_whitespace).nth(2).unwrap_or("").trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
            continue;
        }
        let entry = &line[1..];
        if device_name.is_some() {
            break;
        }
        let id = entry.split_whitespace().next().and_then(|id| u16::from_str_radix(id, 16).ok());
        if id == Some(device) {
            let name = entry.split_once(char::is_whitespace).map(|(_, name)| name.trim()).unwrap_or("");
            device_name = Some(bracketed(name).unwrap_or(name).to_string());
        }
    }
    device_name.filter(|name| !name.is_empty())
}

fn bracketed(name: &str) -> Option<&str> {
    let start = name.find('[')?;
    let end = name[start..].find(']')? + start;
    Some(&name[start + 1..end])
}

fn vendor_word(vendor: u16) -> &'static str {
    match vendor {
        0x1002 => "AMD",
        0x10DE => "NVIDIA",
        0x8086 => "Intel",
        _ => "",
    }
}

/// `AMD`, then the name, unless it says so already.
fn with_vendor(vendor: u16, name: String) -> String {
    let word = vendor_word(vendor);
    if word.is_empty() || name.to_lowercase().starts_with(&word.to_lowercase()) {
        name
    } else {
        format!("{word} {name}")
    }
}

fn adapter_name(vendor: u16, device: u16, revision: u8, sub_vendor: u16, sub_device: u16) -> String {
    let from_libdrm = (vendor == 0x1002)
        .then(|| std::fs::read_to_string("/usr/share/libdrm/amdgpu.ids").ok())
        .flatten()
        .and_then(|table| amdgpu_ids_name(&table, device, revision));
    if let Some(name) = from_libdrm {
        return name;
    }
    let from_pci_ids = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids", "/var/lib/pciutils/pci.ids"]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .and_then(|table| pci_ids_name(&table, vendor, device, sub_vendor, sub_device));
    match from_pci_ids {
        Some(name) => with_vendor(vendor, name),
        None => with_vendor(vendor, format!("Graphics 0x{device:04X}")),
    }
}

fn hex_field(path: &Path, name: &str) -> Option<u32> {
    let text = read_trimmed(path.join(name))?;
    u32::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

fn driver_text(path: &Path) -> Option<String> {
    let driver = std::fs::read_link(path.join("driver")).ok()?;
    let driver = driver.file_name()?.to_str()?.to_string();
    // A driver built outside the kernel says its own version; an in-tree one is
    // the kernel's.
    let version = read_trimmed(format!("/sys/module/{driver}/version")).or_else(kernel_release);
    Some(match version {
        Some(version) => format!("{driver} {version}"),
        None => driver,
    })
}

/// The display functions on the PCI bus, the one driving the screen first.
fn video_controllers() -> Vec<WmiObject> {
    let Ok(devices) = std::fs::read_dir(PCI_ROOT) else {
        return Vec::new();
    };
    let mut adapters: Vec<(bool, String, WmiObject)> = Vec::new();
    for entry in devices.flatten() {
        let path = entry.path();
        if hex_field(&path, "class").is_none_or(|class| class >> 16 != 0x03) {
            continue;
        }
        let (Some(vendor), Some(device)) = (hex_field(&path, "vendor"), hex_field(&path, "device")) else {
            continue;
        };
        let revision = hex_field(&path, "revision").unwrap_or(0);
        let sub_vendor = hex_field(&path, "subsystem_vendor").unwrap_or(0);
        let sub_device = hex_field(&path, "subsystem_device").unwrap_or(0);
        let name = adapter_name(vendor as u16, device as u16, revision as u8, sub_vendor as u16, sub_device as u16);
        let mut adapter = WmiObject::new()
            .with("Caption", name.clone())
            .with("Name", name)
            .with(
                "PNPDeviceID",
                format!(
                    "PCI\\VEN_{vendor:04X}&DEV_{device:04X}&SUBSYS_{sub_device:04X}{sub_vendor:04X}&REV_{revision:02X}\\{}",
                    entry.file_name().to_string_lossy()
                ),
            )
            .with("MatchingDeviceId", format!("pci\\ven_{vendor:04x}&dev_{device:04x}"));
        adapter = with_text(adapter, "DriverVersion", driver_text(&path));
        if let Some(bytes) = read_trimmed(path.join("mem_info_vram_total")).and_then(|text| text.parse::<u64>().ok()) {
            adapter = adapter.with("AdapterRAM", bytes);
        }
        let primary = read_trimmed(path.join("boot_vga")).as_deref() == Some("1");
        adapters.push((primary, entry.file_name().to_string_lossy().into_owned(), adapter));
    }
    adapters.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    adapters.into_iter().map(|(_, _, adapter)| adapter).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Type 17 structure as firmware writes it, then one with no strings, then
    /// the end marker.
    fn sample_table() -> Vec<u8> {
        let mut device = vec![0u8; 0x5C];
        device[0] = 17;
        device[1] = 0x5C;
        device[0x08..0x0A].copy_from_slice(&64u16.to_le_bytes()); // total width
        device[0x0A..0x0C].copy_from_slice(&64u16.to_le_bytes()); // data width
        device[0x0C..0x0E].copy_from_slice(&0x7FFFu16.to_le_bytes()); // size: see extended
        device[0x10] = 1; // locator
        device[0x11] = 2; // bank
        device[0x12] = 0x22; // DDR5
        device[0x15..0x17].copy_from_slice(&0xFFFFu16.to_le_bytes()); // speed: see extended
        device[0x17] = 3; // manufacturer
        device[0x18] = 4; // serial
        device[0x1A] = 5; // part number
        device[0x1B] = 0x02; // two ranks
        device[0x1C..0x20].copy_from_slice(&32768u32.to_le_bytes());
        device[0x20..0x22].copy_from_slice(&8000u16.to_le_bytes());
        device[0x54..0x58].copy_from_slice(&8800u32.to_le_bytes());
        let mut table = device;
        table.extend_from_slice(b"Controller0-ChannelA-DIMM1\0BANK 0\0G.Skill\0ABCD1234\0F5-8800\0\0");

        let mut empty = vec![0u8; 0x5C];
        empty[0] = 17;
        empty[1] = 0x5C;
        table.extend_from_slice(&empty);
        table.extend_from_slice(&[0, 0]);

        let mut array = vec![0u8; 0x17];
        array[0] = 16;
        array[1] = 0x17;
        array[0x07..0x0B].copy_from_slice(&0x8000_0000u32.to_le_bytes());
        array[0x0D..0x0F].copy_from_slice(&2u16.to_le_bytes());
        array[0x0F..0x17].copy_from_slice(&(256u64 << 30).to_le_bytes());
        table.extend_from_slice(&array);
        table.extend_from_slice(&[0, 0]);

        table.extend_from_slice(&[127, 4, 0, 0, 0, 0]);
        table
    }

    #[test]
    fn the_walk_finds_every_structure_and_its_strings() {
        let structures = parse_smbios(&sample_table());
        assert_eq!(structures.iter().map(|s| s.kind).collect::<Vec<_>>(), vec![17, 17, 16, 127]);
        assert_eq!(structures[0].string(0x10).as_deref(), Some("Controller0-ChannelA-DIMM1"));
        assert_eq!(structures[0].string(0x1A).as_deref(), Some("F5-8800"));
        assert_eq!(structures[1].string(0x10), None, "index 0 means no string");
    }

    #[test]
    fn a_truncated_table_ends_the_walk_rather_than_reading_past_it() {
        let mut table = sample_table();
        table.truncate(0x20);
        assert!(parse_smbios(&table).is_empty());
        assert!(parse_smbios(&[17, 2, 0, 0]).is_empty(), "a length under the header");
    }

    #[test]
    fn modules_read_as_wmi_describes_them() {
        let modules = physical_memory_from(&parse_smbios(&sample_table()));
        assert_eq!(modules.len(), 1, "an empty socket is not an object");
        let module = &modules[0];
        assert_eq!(module.uint("Capacity"), Some(32 * 1024 * 1024 * 1024));
        assert_eq!(module.text("DeviceLocator").as_deref(), Some("Controller0-ChannelA-DIMM1"));
        assert_eq!(module.text("BankLabel").as_deref(), Some("BANK 0"));
        assert_eq!(module.text("Manufacturer").as_deref(), Some("G.Skill"));
        assert_eq!(module.text("SerialNumber").as_deref(), Some("ABCD1234"));
        assert_eq!(module.text("PartNumber").as_deref(), Some("F5-8800"));
        assert_eq!(module.int("SMBIOSMemoryType"), Some(0x22));
        assert_eq!(module.int("Attributes"), Some(2));
        assert_eq!(module.int("Speed"), Some(8800), "0xFFFF defers to the extended field");
        assert_eq!(module.int("ConfiguredClockSpeed"), Some(8000));
        assert_eq!(module.text("Tag").as_deref(), Some("Physical Memory 0"));
    }

    #[test]
    fn an_array_reports_its_sockets_and_extended_capacity() {
        let arrays = physical_memory_array_from(&parse_smbios(&sample_table()));
        assert_eq!(arrays.len(), 1);
        assert_eq!(arrays[0].int("MemoryDevices"), Some(2));
        assert_eq!(arrays[0].uint("MaxCapacity"), Some(256 << 20), "kilobytes");
    }

    #[test]
    fn capacity_follows_the_size_fields_rules() {
        assert_eq!(module_capacity(0, None), None);
        assert_eq!(module_capacity(0xFFFF, None), None, "unknown");
        assert_eq!(module_capacity(8192, None), Some(8192 << 20), "megabytes");
        assert_eq!(module_capacity(0x8000 | 512, None), Some(512 << 10), "bit 15 means kilobytes");
        assert_eq!(module_capacity(0x7FFF, Some(65536)), Some(65536 << 20));
        assert_eq!(module_capacity(0x7FFF, None), None);
    }

    #[test]
    fn cpuinfo_stops_at_the_first_processor() {
        let text = "processor\t: 0\nvendor_id\t: GenuineIntel\nmodel name\t: Intel(R) Core(TM) Ultra 7 270K Plus\n\nprocessor\t: 1\nmodel name\t: other\n";
        let fields = cpuinfo_fields(text);
        assert_eq!(fields["vendor_id"], "GenuineIntel");
        assert_eq!(fields["model name"], "Intel(R) Core(TM) Ultra 7 270K Plus");
    }

    #[test]
    fn cpu_lists_expand() {
        assert_eq!(parse_cpu_list("0-3,8,10-11\n"), vec![0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(parse_cpu_list(""), Vec::<u32>::new());
        assert_eq!(parse_cpu_list("5"), vec![5]);
    }

    #[test]
    fn the_processor_id_decodes_as_wmis_does() {
        // What this project's decoder expects: leaf 1 EAX in the last eight.
        let (family, model, stepping) =
            crate::system_identity::decode_wmi_processor_signature(Some("BFEBFBFF000C0662")).unwrap();
        assert_eq!((family, model, stepping), (6, 0xC6, 2));
    }

    #[test]
    fn dates_become_wmi_datetimes() {
        assert_eq!(cim_date("04/29/2026").as_deref(), Some("20260429000000.000000+000"));
        assert_eq!(cim_date("not a date"), None);
    }

    #[test]
    fn memory_total_is_in_bytes() {
        assert_eq!(mem_total_bytes("MemFree: 1 kB\nMemTotal:       65532704 kB\n"), Some(65532704 * 1024));
        assert_eq!(mem_total_bytes("nothing"), None);
    }

    #[test]
    fn the_os_caption_carries_the_version_once() {
        let omarchy = "NAME=\"Omarchy\"\nPRETTY_NAME=\"Omarchy\"\nVERSION_ID=\"4.0.6\"\n";
        assert_eq!(os_caption(omarchy), "Omarchy 4.0.6");
        let fedora = "PRETTY_NAME=\"Fedora Linux 43 (Workstation)\"\nVERSION_ID=43\n";
        assert_eq!(os_caption(fedora), "Fedora Linux 43 (Workstation)");
        assert_eq!(os_caption(""), "Linux");
    }

    #[test]
    fn libdrm_tells_the_cuts_of_one_device_apart() {
        let table = "# comment\n1.0.0\n744C,\tC8,\tAMD Radeon RX 7900 XTX\n744C,\tCC,\tAMD Radeon RX 7900 XT\n";
        assert_eq!(amdgpu_ids_name(table, 0x744C, 0xCC).as_deref(), Some("AMD Radeon RX 7900 XT"));
        assert_eq!(amdgpu_ids_name(table, 0x744C, 0xC8).as_deref(), Some("AMD Radeon RX 7900 XTX"));
        assert_eq!(amdgpu_ids_name(table, 0x744C, 0x00), None);
    }

    const PCI_IDS: &str = "# comment\n\
10de  NVIDIA Corporation\n\
\t2684  AD102 [GeForce RTX 4090]\n\
\t\t10de 16f3  Founders Edition\n\
\t2704  AD103 [GeForce RTX 4080]\n\
1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\
\t744c  Navi 31 [Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M]\n\
\t\t1da2 475e  PULSE RX 7900 GRE\n\
\t73ff  Navi 23\n\
1022  Advanced Micro Devices, Inc. [AMD]\n";

    #[test]
    fn pci_ids_prefers_the_subsystem_then_the_bracketed_product() {
        assert_eq!(pci_ids_name(PCI_IDS, 0x10DE, 0x2684, 0x10DE, 0x16F3).as_deref(), Some("Founders Edition"));
        assert_eq!(pci_ids_name(PCI_IDS, 0x10DE, 0x2684, 0x1458, 0x0000).as_deref(), Some("GeForce RTX 4090"));
        assert_eq!(
            pci_ids_name(PCI_IDS, 0x1002, 0x744C, 0x1DA2, 0x471E).as_deref(),
            Some("Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M"),
        );
        assert_eq!(pci_ids_name(PCI_IDS, 0x1002, 0x73FF, 0, 0).as_deref(), Some("Navi 23"));
        assert_eq!(pci_ids_name(PCI_IDS, 0x1002, 0x0001, 0, 0), None);
        assert_eq!(pci_ids_name(PCI_IDS, 0x8086, 0x0001, 0, 0), None);
    }

    #[test]
    fn the_vendor_is_named_once() {
        assert_eq!(with_vendor(0x10DE, "GeForce RTX 4090".into()), "NVIDIA GeForce RTX 4090");
        assert_eq!(with_vendor(0x8086, "Intel Graphics".into()), "Intel Graphics");
        assert_eq!(with_vendor(0x1234, "Mystery".into()), "Mystery");
    }

    #[test]
    fn an_unknown_class_is_an_error_not_an_empty_answer() {
        assert!(query("Win32_Fan").is_err());
    }
}
