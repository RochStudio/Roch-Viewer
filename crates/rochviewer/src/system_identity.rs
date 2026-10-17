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

//! Identity readings that are the same question on any platform.
//!
//! The memory generation, the microcode revision and the Super I/O on the LPC
//! bus are facts about the machine rather than about its memory controller, and
//! they are read the same way whoever made the CPU: SMBIOS, the registry, and
//! the chip's own ID port. They live here so both platform profiles can ask for
//! them, rather than one of them owning readings the other also needs.
//!
//! The reads live here; the tables do not. Naming silicon from a device ID is a
//! per-platform claim -- Intel's host bridge table answers on an AMD board too,
//! and would name 0x14D8 as an Intel part -- so each profile keeps its own table
//! and this module only supplies the reading.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::hardware::pci_mcfg::{default_allocation, ecam_address};
use crate::hardware::read::read_physical_memory_int;
use crate::sensors::board_sensors::detected_lpcio_chip;
use crate::wmi_util;

pub use registry::RegValue;

/// SMBIOS Type 17 memory-type codes, which are numbers rather than names in
/// the firmware. Only the generations this tool runs on are decoded; anything
/// else is reported as its code so an unknown board still says something true.
pub const SMBIOS_MEMORY_TYPES: &[(i64, &str)] = &[
    (0x18, "DDR3"),
    (0x1A, "DDR4"),
    (0x22, "DDR5"),
    (0x23, "LPDDR5"),
];

pub const MICROCODE_REGISTRY_PATH: &str = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";

/// Which vendor each Super I/O reader speaks for. The chip names the readers
/// carry -- NCT6687D, NCT6798D, IT8696E -- do not say who makes them, and the
/// row is read as "LPCIO" the way a board's spec sheet writes it.
pub const LPCIO_VENDORS: &[(&str, &str)] = &[
    ("superio_lpc", "Nuvoton"),
    ("nct679x", "Nuvoton"),
    ("ite_superio", "ITE"),
];

pub const WINDOWS_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

/// Windows' PNP device id carries the four PCI fields already, so a display
/// adapter can be identified without walking configuration space.
static PNP_IDENTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)VEN_([0-9A-F]{4})&DEV_([0-9A-F]{4})&SUBSYS_([0-9A-F]{4})([0-9A-F]{4})&REV_([0-9A-F]{2})",
    )
    .expect("the PNP identity pattern is valid")
});

/// PCI subsystem vendors: who assembled the board, whoever made the chip on it.
/// 0x148C is measured on this bench, where the card is a PowerColor; the rest
/// are the other major add-in-board vendors. An unlisted vendor prints its ID
/// rather than a guess.
pub const BOARD_VENDORS: &[(u16, &str)] = &[
    (0x1002, "AMD"),
    (0x1043, "ASUSTeK Computer"),
    (0x1458, "GIGABYTE Technology"),
    (0x1462, "MSI"),
    (0x148C, "PowerColor"),
    (0x1569, "Palit Microsystems"),
    (0x174B, "Sapphire Technology"),
    (0x196E, "PNY"),
    (0x19DA, "Zotac"),
    (0x1DA2, "Sapphire Technology"),
    (0x1EAE, "XFX"),
    (0x3842, "EVGA"),
    (0x10DE, "NVIDIA"),
];

/// A display adapter's PCI identity, as its PNP device id carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdapterIdentity {
    pub vendor_id: u16,
    pub device_id: u16,
    pub subsystem_vendor_id: u16,
    pub subsystem_device_id: u16,
    pub revision: u8,
}

/// A PCIe link as the root port reports it. See [`pcie_link`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcieLink {
    pub max_gen: u32,
    pub max_width: u32,
    /// The speed the link runs at now (the Python key `gen`; `gen` is reserved
    /// in Rust 2024).
    pub generation: u32,
    pub width: u32,
}

/// A PCI function's location: `(bus, device, function)`.
pub type PciLocation = (u8, u8, u8);

/// The version key's values, read in one open. None of them can change.
///
/// Keyed by value name ("UBR", "DisplayVersion", "ReleaseId"); a value the
/// key does not hold is absent, and an unreadable key is an empty map.
pub fn windows_version_values() -> BTreeMap<&'static str, RegValue> {
    let mut found = BTreeMap::new();
    let key = match registry::RegKey::open_local_machine(WINDOWS_VERSION_KEY) {
        Ok(key) => key,
        Err(_) => return BTreeMap::new(),
    };
    for name in ["UBR", "DisplayVersion", "ReleaseId"] {
        if let Ok(value) = key.query_value(name) {
            found.insert(name, value);
        }
    }
    found
}

/// Windows edition, architecture and full build, as CPU-Z states it.
///
/// Its About tab reads "Microsoft Windows 11 Professional (x64) 23H2 Build
/// 22631.6199", which is the same facts WMI carries plus the update revision
/// -- so the parts are assembled rather than the caption printed as-is.
/// "Pro" is spelled the way the edition is: WMI abbreviates it in the caption
/// while the edition itself is Professional.
pub fn os_name() -> Option<String> {
    let systems = wmi_util::query("Win32_OperatingSystem").ok()?;
    let system = systems.first()?;
    let mut caption = py_strip(&system.str_or_empty("Caption")).to_string();
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
    let architecture = system.str_or_empty("OSArchitecture");
    let architecture = py_strip(&architecture);
    if !architecture.is_empty() {
        // WMI says "64-bit"; CPU-Z, and everyone else, says x64.
        caption += if architecture.contains("64") { " (x64)" } else { " (x86)" };
    }
    let values = windows_version_values();
    // winver's ordering: the feature update, then the build it produced.
    let display_version = values
        .get("DisplayVersion")
        .filter(|value| value.is_truthy())
        .or_else(|| values.get("ReleaseId"))
        .filter(|value| value.is_truthy());
    if let Some(display_version) = display_version {
        caption += &format!(" {}", display_version.py_str());
    }
    let build = system.str_or_empty("BuildNumber");
    let build = py_strip(&build);
    if !build.is_empty() {
        caption += &format!(" Build {build}");
        if let Some(RegValue::Int(revision)) = values.get("UBR") {
            caption += &format!(".{revision}");
        }
    }
    Some(caption)
}

/// The display adapter's PCI identity, or `None`.
///
/// From the PNP device id Windows already holds, which carries vendor,
/// device, subsystem and revision. Walking configuration space for the same
/// four cost a map/unmap pair per dword across 64 functions and returned the
/// first VGA-class device it found -- the integrated graphics, on a machine
/// that has both.
///
/// `None` for the ids asks WMI for the display adapters.
pub fn adapter_identity(pnp_device_ids: Option<&[String]>) -> Option<AdapterIdentity> {
    let own;
    let ids = match pnp_device_ids {
        Some(ids) => ids,
        None => {
            own = wmi_util::query("Win32_VideoController")
                .ok()?
                .iter()
                .filter(|adapter| !adapter.str_or_empty("Name").contains("Basic Display"))
                .map(|adapter| adapter.str_or_empty("PNPDeviceID"))
                .collect::<Vec<_>>();
            &own
        }
    };
    ids.iter().find_map(|id| parse_pnp_identity(&PNP_IDENTITY, id))
}

/// Decode one PNP device id with `pattern`, or `None` when it is not a PCI id.
pub(crate) fn parse_pnp_identity(pattern: &Regex, pnp_device_id: &str) -> Option<AdapterIdentity> {
    let found = pattern.captures(pnp_device_id)?;
    let hex16 = |group: usize| u16::from_str_radix(&found[group], 16).ok();
    Some(AdapterIdentity {
        vendor_id: hex16(1)?,
        device_id: hex16(2)?,
        // SUBSYS is device then vendor, high half first.
        subsystem_vendor_id: hex16(4)?,
        subsystem_device_id: hex16(3)?,
        revision: u8::from_str_radix(&found[5], 16).ok()?,
    })
}

/// Who assembled the board, or its ID when the table does not name it.
pub fn board_vendor(subsystem_vendor_id: Option<u16>) -> Option<String> {
    let vendor = subsystem_vendor_id?;
    Some(
        BOARD_VENDORS
            .iter()
            .find(|(id, _)| *id == vendor)
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| format!("0x{vendor:04X}")),
    )
}

#[cfg(test)]
type ConfigSpaceReader = Box<dyn Fn(u8, u8, u32, u8) -> Option<u32>>;

#[cfg(test)]
thread_local! {
    static CONFIG_SPACE: std::cell::RefCell<Option<ConfigSpaceReader>> =
        const { std::cell::RefCell::new(None) };
}

/// Answer [`pci_config_dword`] from `reader(device, function, offset, bus)`
/// on the calling test thread; `None` goes back to the ECAM read.
#[cfg(test)]
pub(crate) fn set_thread_config_space(reader: Option<ConfigSpaceReader>) {
    CONFIG_SPACE.with(|slot| *slot.borrow_mut() = reader);
}

/// Read one dword from a PCI function's configuration space, or `None`.
///
/// Through the firmware's own ECAM window, which is neither an Intel nor an
/// AMD idea -- both platforms name their silicon by reading the same two
/// registers here, so the read lives once and each profile brings its own
/// table of what the identifiers mean.
pub fn pci_config_dword(device: u8, function: u8, offset: u32, bus: u8) -> Option<u32> {
    #[cfg(test)]
    {
        let faked = CONFIG_SPACE.with(|slot| {
            slot.borrow().as_ref().map(|reader| reader(device, function, offset, bus))
        });
        if let Some(value) = faked {
            return value;
        }
    }
    let allocation = default_allocation(0, bus).ok()?;
    let register = u16::try_from(offset).ok()?;
    let address = ecam_address(&allocation, bus, device, function, register).ok()?;
    read_physical_memory_int(address, 4).map(|value| value as u32)
}

pub const PCI_HEADER_BRIDGE: u32 = 1;
pub const PCI_CLASS_DISPLAY: u32 = 0x03;

/// Where the extended capability list starts, and the Resizable BAR entry's id.
pub const PCI_EXTENDED_CAPABILITIES: u32 = 0x100;
pub const RESIZABLE_BAR_CAPABILITY: u32 = 0x0015;
pub const MAX_CAPABILITY_HOPS: usize = 32;

/// Walk down the bridges to a display function, or `None`.
///
/// The card is not on bus 0 -- here it sits three bridges down, at 03:00.0 --
/// and the PNP id Windows hands out does not carry the bus. Each bridge names
/// the bus behind it, so the walk follows those rather than sweeping 256
/// buses for a device that is on one of them.
///
/// Start the walk at `bus` 0, `depth` 0.
pub fn find_display_function(vendor_id: u16, bus: u8, depth: u32) -> Option<PciLocation> {
    find_display_path(vendor_id, bus, depth).and_then(|path| path.last().copied())
}

/// The bridges from bus 0 down to a display function, then the function.
///
/// A list of `(bus, device, function)`, the root port first and the card
/// last, or `None`. The bridges matter for the link: a Radeon sits behind a
/// PCIe switch on its own board, so the card's own link status describes the
/// switch's internal link. The slot's link is the root port's.
///
/// Start the walk at `bus` 0, `depth` 0.
pub fn find_display_path(vendor_id: u16, bus: u8, depth: u32) -> Option<Vec<PciLocation>> {
    if depth > 6 {
        return None;
    }
    for device in 0..32u8 {
        for function in 0..8u8 {
            let identity = match pci_config_dword(device, function, 0x00, bus) {
                Some(identity) if identity != 0 && identity != 0xFFFF_FFFF => identity,
                _ => continue,
            };
            let classcode = pci_config_dword(device, function, 0x08, bus).unwrap_or(0) >> 16;
            if (classcode >> 8) == PCI_CLASS_DISPLAY && (identity & 0xFFFF) == vendor_id as u32 {
                return Some(vec![(bus, device, function)]);
            }
            let header = (pci_config_dword(device, function, 0x0C, bus).unwrap_or(0) >> 16) & 0x7F;
            if header != PCI_HEADER_BRIDGE {
                continue;
            }
            let secondary = ((pci_config_dword(device, function, 0x18, bus).unwrap_or(0) >> 8)
                & 0xFF) as u8;
            if secondary != 0 && secondary != bus {
                if let Some(found) = find_display_path(vendor_id, secondary, depth + 1) {
                    let mut path = vec![(bus, device, function)];
                    path.extend(found);
                    return Some(path);
                }
            }
        }
    }
    None
}

/// The PCI Express capability, and the port type its capabilities register
/// gives a root port.
pub const PCI_EXPRESS_CAPABILITY: u32 = 0x10;
pub const PCIE_ROOT_PORT: u32 = 4;

/// The offset of one standard capability in a function's list, or `None`.
pub fn pci_capability(location: PciLocation, capability_id: u32) -> Option<u32> {
    let (bus, device, function) = location;
    let mut pointer = pci_config_dword(device, function, 0x34, bus).unwrap_or(0) & 0xFC;
    for _hop in 0..MAX_CAPABILITY_HOPS {
        if pointer == 0 {
            return None;
        }
        let header = pci_config_dword(device, function, pointer, bus)?;
        if header == 0xFFFF_FFFF {
            return None;
        }
        if header & 0xFF == capability_id {
            return Some(pointer);
        }
        pointer = (header >> 8) & 0xFC;
    }
    None
}

/// The slot's PCIe link, or `None`.
///
/// Read at the root port, which is the CPU's end of the slot: its status
/// register gives the link as it runs now, and the link can train no higher
/// than the lower of its capability and that of the device on the other end.
/// Nothing is written. The generation is the speed code, 1 for 2.5 GT/s up
/// to 5 for 32 GT/s.
pub fn pcie_link(path: Option<&[PciLocation]>) -> Option<PcieLink> {
    let path = path?;
    if path.len() < 2 {
        return None;
    }
    let (root, below) = (path[0], path[1]);
    let root_cap = pci_capability(root, PCI_EXPRESS_CAPABILITY);
    let below_cap = pci_capability(below, PCI_EXPRESS_CAPABILITY);
    let (Some(root_cap), Some(below_cap)) = (root_cap, below_cap) else {
        return None;
    };
    let (bus, device, function) = root;
    let port_type = (pci_config_dword(device, function, root_cap, bus).unwrap_or(0) >> 20) & 0xF;
    if port_type != PCIE_ROOT_PORT {
        return None;
    }
    let root_link = pci_config_dword(device, function, root_cap + 0x0C, bus);
    let status = pci_config_dword(device, function, root_cap + 0x10, bus);
    let below_link = pci_config_dword(below.1, below.2, below_cap + 0x0C, below.0);
    let (Some(root_link), Some(status), Some(below_link)) = (root_link, status, below_link) else {
        return None;
    };
    let status = status >> 16;
    let link = PcieLink {
        max_gen: (root_link & 0xF).min(below_link & 0xF),
        max_width: ((root_link >> 4) & 0x3F).min((below_link >> 4) & 0x3F),
        generation: status & 0xF,
        width: (status >> 4) & 0x3F,
    };
    if link.max_gen == 0 || link.max_width == 0 || link.generation == 0 || link.width == 0 {
        return None;
    }
    Some(link)
}

/// The size the card's first BAR is currently programmed to, in MB.
///
/// From the Resizable BAR capability's control register, which reports the
/// size in use. Nothing is written: the usual way to size a BAR is to write
/// all ones and read back the mask, and this tool does not write to PCI
/// configuration space. A card without the capability returns `None`, which
/// is not the same as a card that has it and is set to the 256 MB default.
pub fn resizable_bar_megabytes(location: Option<PciLocation>) -> Option<u64> {
    let (bus, device, function) = location?;
    let mut offset = PCI_EXTENDED_CAPABILITIES;
    for _hop in 0..MAX_CAPABILITY_HOPS {
        let header = pci_config_dword(device, function, offset, bus)?;
        if header == 0 || header == 0xFFFF_FFFF {
            return None;
        }
        if (header & 0xFFFF) == RESIZABLE_BAR_CAPABILITY {
            let control = pci_config_dword(device, function, offset + 0x08, bus)?;
            return Some(1u64 << ((control >> 8) & 0x3F));
        }
        offset = (header >> 20) & 0xFFF;
        if offset == 0 {
            return None;
        }
    }
    None
}

/// `(device id, revision)` for one PCI function on bus 0, or `None`.
pub fn pci_device_and_revision(device: u8, function: u8) -> Option<(u16, u8)> {
    let identity = pci_config_dword(device, function, 0x00, 0)?;
    if identity == 0 || identity == 0xFFFF_FFFF {
        return None;
    }
    let revision = (pci_config_dword(device, function, 0x08, 0).unwrap_or(0) & 0xFF) as u8;
    Some((((identity >> 16) & 0xFFFF) as u16, revision))
}

/// Decode CPUID family, model and stepping from WMI leaf-1 EAX.
///
/// Here rather than beside the probe that first needed it. It is pure
/// arithmetic on a string, but it lived in amd_smu_version_probe -- a
/// write-capable research tool that must never ship -- and one import of it
/// from intel_timings was enough to pull that whole module into the release
/// build. A shared decoder belongs somewhere shippable.
pub fn decode_wmi_processor_signature(processor_id: Option<&str>) -> Result<(u32, u32, u32), String> {
    let text = py_strip(processor_id.unwrap_or(""));
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 8 {
        return Err("WMI ProcessorId is unavailable".into());
    }
    let tail: String = chars[chars.len() - 8..].iter().collect();
    let eax = py_int(&tail, 16).ok_or_else(|| "WMI ProcessorId is invalid".to_string())?;
    let base_family = (eax >> 8) & 0xF;
    let extended_family = (eax >> 20) & 0xFF;
    let family = if base_family == 0xF { base_family + extended_family } else { base_family };
    let base_model = (eax >> 4) & 0xF;
    let extended_model = (eax >> 16) & 0xF;
    let model = if base_family == 0x6 || base_family == 0xF {
        base_model | (extended_model << 4)
    } else {
        base_model
    };
    let stepping = eax & 0x0F;
    Ok((family as u32, model as u32, stepping as u32))
}

/// Decode displayed CPUID family/model from WMI ProcessorId leaf-1 EAX.
pub fn decode_wmi_processor_id(processor_id: Option<&str>) -> Result<(u32, u32), String> {
    let (family, model, _stepping) = decode_wmi_processor_signature(processor_id)?;
    Ok((family, model))
}

/// The DDR generation, as SMBIOS states it, or `None`.
pub fn memory_type() -> Option<String> {
    let modules = wmi_util::query("Win32_PhysicalMemory").ok()?;
    let mut found: Vec<String> = Vec::new();
    for memory in &modules {
        let code = match memory.get("SMBIOSMemoryType") {
            None | Some(wmi_util::WmiValue::Null) => continue,
            // int() on something that is not a number raised, and the whole
            // reading answered nothing.
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
}

/// The revision out of the registry's Update Revision value.
///
/// Windows commonly stores the revision in the upper 32 bits, so 0x13300000000
/// means revision 0x133. Split apart rather than printed whole, which is how
/// this row used to disagree with every other tool.
///
/// Bytes are read little-endian; an integer as itself; anything else is
/// `None`.
pub fn decode_microcode(value: &RegValue) -> Option<String> {
    let bytes = match value {
        RegValue::Bytes(bytes) => bytes.clone(),
        RegValue::Int(revision) => revision.to_le_bytes().to_vec(),
        _ => return None,
    };
    let dword = |start: usize| {
        (0..4).fold(0u32, |acc, i| {
            acc | ((bytes.get(start + i).copied().unwrap_or(0) as u32) << (8 * i))
        })
    };
    let lower = dword(0);
    // The value exceeds 0xFFFFFFFF exactly when a byte above the low four is
    // set, however long the value is.
    let revision = if bytes.iter().skip(4).any(|b| *b != 0) {
        let upper = dword(4);
        if upper != 0 { upper } else { lower }
    } else {
        lower
    };
    Some(format!("0x{revision:X}"))
}

/// The running microcode revision, or `None`.
pub fn microcode() -> Option<String> {
    let key = registry::RegKey::open_local_machine(MICROCODE_REGISTRY_PATH).ok()?;
    let value = key.query_value("Update Revision").ok()?;
    decode_microcode(&value)
}

/// The Super I/O on the LPC bus, by its chip ID, or `None`.
///
/// Through the board sensors' own profile, which is the thing that already
/// decided which chip is here. Probing separately would unlock and re-lock
/// the configuration window under the monitoring mutex for an answer that
/// cannot change. The vendor comes from the reader that answered rather than
/// from the chip name, which carries no vendor of its own.
pub fn lpcio_name() -> Option<String> {
    lpcio_from(detected_lpcio_chip())
}

fn lpcio_from(chip: Option<(&str, String)>) -> Option<String> {
    let (module, name) = chip?;
    if name.is_empty() {
        return None;
    }
    // The last component, not the whole dotted path: __module__ became
    // "rochviewer.sensors.superio_lpc" when the modules moved into folders,
    // and a lookup on the full name silently stopped matching -- the row kept
    // naming the chip and quietly dropped the vendor in front of it.
    let module = module.rsplit('.').next().unwrap_or(module);
    match LPCIO_VENDORS.iter().find(|(reader, _)| *reader == module) {
        Some((_, vendor)) => Some(format!("{vendor} {name}")),
        None => Some(name),
    }
}

/// Python's `str.strip()` with no argument: Unicode whitespace plus the
/// four separator controls Python also counts as space.
pub(crate) fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// Python's `int(text, radix)` for radix 10 or 16: surrounding whitespace,
/// a sign, a `0x` prefix in base 16, and single underscores between digits.
pub(crate) fn py_int(text: &str, radix: u32) -> Option<i64> {
    let text = py_strip(text);
    let (negative, rest) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let (prefixed, digits) = if radix == 16 && (rest.starts_with("0x") || rest.starts_with("0X")) {
        (true, &rest[2..])
    } else {
        (false, rest)
    };
    let digits = if prefixed { digits.strip_prefix('_').unwrap_or(digits) } else { digits };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    // One sign, and only ahead of the prefix: int("+-5") and int("0x-5", 16)
    // both raise, where from_str_radix would take the second sign.
    if cleaned.starts_with(['+', '-']) {
        return None;
    }
    let value = i64::from_str_radix(&cleaned, radix).ok()?;
    Some(if negative { -value } else { value })
}

/// The Windows registry, read-only: the parts of `winreg` this project uses.
pub mod registry {
    use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_MULTI_SZ, REG_QWORD,
        REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
        RegQueryInfoKeyW, RegQueryValueExW,
    };
    use windows::core::{PCWSTR, PWSTR};

    /// A registry value converted the way Python's `winreg` converts it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum RegValue {
        /// An empty value of a type `winreg` returns as bytes (`None` there).
        None,
        /// REG_DWORD and REG_QWORD.
        Int(u64),
        /// REG_SZ and REG_EXPAND_SZ (unexpanded), up to the first NUL.
        Text(String),
        /// REG_MULTI_SZ.
        Multi(Vec<String>),
        /// REG_BINARY and every other type.
        Bytes(Vec<u8>),
    }

    impl RegValue {
        /// `winreg`'s conversion of a value's type and raw data.
        pub fn from_raw(kind: REG_VALUE_TYPE, data: &[u8]) -> RegValue {
            let le = |width: usize| {
                data.iter()
                    .take(width)
                    .enumerate()
                    .fold(0u64, |acc, (i, b)| acc | ((*b as u64) << (8 * i)))
            };
            let wide: Vec<u16> = data
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            match kind {
                REG_DWORD => RegValue::Int(le(4)),
                REG_QWORD => RegValue::Int(le(8)),
                REG_SZ | REG_EXPAND_SZ => {
                    let end = wide.iter().position(|c| *c == 0).unwrap_or(wide.len());
                    RegValue::Text(String::from_utf16_lossy(&wide[..end]))
                }
                REG_MULTI_SZ => RegValue::Multi(
                    wide.split(|c| *c == 0)
                        .take_while(|part| !part.is_empty())
                        .map(String::from_utf16_lossy)
                        .collect(),
                ),
                _ if data.is_empty() => RegValue::None,
                _ => RegValue::Bytes(data.to_vec()),
            }
        }

        /// Python truthiness of the converted value.
        pub fn is_truthy(&self) -> bool {
            match self {
                RegValue::None => false,
                RegValue::Int(value) => *value != 0,
                RegValue::Text(text) => !text.is_empty(),
                RegValue::Multi(items) => !items.is_empty(),
                RegValue::Bytes(bytes) => !bytes.is_empty(),
            }
        }

        /// Python's `str()` of the converted value.
        pub fn py_str(&self) -> String {
            match self {
                RegValue::None => "None".into(),
                RegValue::Int(value) => value.to_string(),
                RegValue::Text(text) => text.clone(),
                RegValue::Multi(items) => format!(
                    "[{}]",
                    items.iter().map(|item| str_repr(item)).collect::<Vec<_>>().join(", ")
                ),
                RegValue::Bytes(bytes) => bytes_repr(bytes),
            }
        }
    }

    fn str_repr(text: &str) -> String {
        let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
        let mut out = String::from(quote);
        for c in text.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c == quote => {
                    out.push('\\');
                    out.push(c);
                }
                c if (c as u32) < 0x20 || c as u32 == 0x7F => out.push_str(&format!("\\x{:02x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push(quote);
        out
    }

    fn bytes_repr(bytes: &[u8]) -> String {
        let quote = if bytes.contains(&b'\'') && !bytes.contains(&b'"') { b'"' } else { b'\'' };
        let mut out = String::from("b");
        out.push(quote as char);
        for &b in bytes {
            match b {
                b'\\' => out.push_str("\\\\"),
                b'\n' => out.push_str("\\n"),
                b'\r' => out.push_str("\\r"),
                b'\t' => out.push_str("\\t"),
                b if b == quote => {
                    out.push('\\');
                    out.push(b as char);
                }
                0x20..=0x7E => out.push(b as char),
                b => out.push_str(&format!("\\x{b:02x}")),
            }
        }
        out.push(quote as char);
        out
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn check(status: WIN32_ERROR, what: &str) -> Result<(), String> {
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("{what} failed: error {}", status.0))
        }
    }

    /// An open key, closed on drop.
    pub struct RegKey(HKEY);

    impl RegKey {
        /// `winreg.OpenKey(HKEY_LOCAL_MACHINE, path)` with read access.
        pub fn open_local_machine(path: &str) -> Result<RegKey, String> {
            if cfg!(test) {
                // Unit tests never read the machine they run on.
                return Err("the registry is not read in unit tests".into());
            }
            open(HKEY_LOCAL_MACHINE, path)
        }

        /// A subkey of this key, with read access.
        pub fn open(&self, path: &str) -> Result<RegKey, String> {
            open(self.0, path)
        }

        /// `winreg.QueryValueEx(key, name)[0]`.
        pub fn query_value(&self, name: &str) -> Result<RegValue, String> {
            let name = wide(name);
            let mut size = 0u32;
            loop {
                let mut kind = REG_VALUE_TYPE(0);
                let mut data = vec![0u8; size as usize];
                let mut actual = size;
                // SAFETY: the key is open, the name is NUL-terminated, and
                // `data` holds `actual` writable bytes (none when `actual` is
                // 0, in which case no data pointer is passed).
                let status = unsafe {
                    RegQueryValueExW(
                        self.0,
                        PCWSTR(name.as_ptr()),
                        None,
                        Some(&mut kind),
                        if size == 0 { None } else { Some(data.as_mut_ptr()) },
                        Some(&mut actual),
                    )
                };
                if status == ERROR_MORE_DATA || (status == ERROR_SUCCESS && size == 0 && actual > 0) {
                    size = actual.max(size.saturating_mul(2)).max(16);
                    continue;
                }
                check(status, "RegQueryValueExW")?;
                data.truncate(actual as usize);
                return Ok(RegValue::from_raw(kind, &data));
            }
        }

        /// `winreg.EnumKey(key, index)`.
        pub fn enum_key(&self, index: u32) -> Result<String, String> {
            // A key name is at most 255 characters.
            let mut name = vec![0u16; 256];
            let mut length = name.len() as u32;
            // SAFETY: the key is open and `name` holds `length` UTF-16 units.
            let status = unsafe {
                RegEnumKeyExW(
                    self.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut length,
                    None,
                    None,
                    None,
                    None,
                )
            };
            check(status, "RegEnumKeyExW")?;
            Ok(String::from_utf16_lossy(&name[..length as usize]))
        }

        /// The number of values the key holds: `winreg.QueryInfoKey(key)[1]`.
        pub fn value_count(&self) -> Result<u32, String> {
            Ok(self.info()?.0)
        }

        /// (value count, longest value name, longest value data).
        fn info(&self) -> Result<(u32, u32, u32), String> {
            let (mut values, mut name_length, mut data_length) = (0u32, 0u32, 0u32);
            // SAFETY: the key is open and every out pointer is a live local.
            let status = unsafe {
                RegQueryInfoKeyW(
                    self.0,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(&mut values),
                    Some(&mut name_length),
                    Some(&mut data_length),
                    None,
                    None,
                )
            };
            check(status, "RegQueryInfoKeyW")?;
            Ok((values, name_length, data_length))
        }

        /// `winreg.EnumValue(key, index)`: the value's name and its value.
        pub fn enum_value(&self, index: u32) -> Result<(String, RegValue), String> {
            let (_, mut name_capacity, mut data_capacity) = self.info()?;
            loop {
                let mut name = vec![0u16; name_capacity as usize + 1];
                let mut name_length = name.len() as u32;
                let mut data = vec![0u8; data_capacity.max(1) as usize];
                let mut data_length = data.len() as u32;
                let mut kind = 0u32;
                // SAFETY: the key is open; `name` holds `name_length` UTF-16
                // units and `data` holds `data_length` bytes.
                let status = unsafe {
                    RegEnumValueW(
                        self.0,
                        index,
                        Some(PWSTR(name.as_mut_ptr())),
                        &mut name_length,
                        None,
                        Some(&mut kind),
                        Some(data.as_mut_ptr()),
                        Some(&mut data_length),
                    )
                };
                if status == ERROR_MORE_DATA {
                    // The key grew between the size query and the read.
                    name_capacity = name_capacity.saturating_mul(2).max(256);
                    data_capacity = data_length.max(data_capacity.saturating_mul(2)).max(256);
                    continue;
                }
                check(status, "RegEnumValueW")?;
                data.truncate(data_length as usize);
                return Ok((
                    String::from_utf16_lossy(&name[..name_length as usize]),
                    RegValue::from_raw(REG_VALUE_TYPE(kind), &data),
                ));
            }
        }
    }

    fn open(parent: HKEY, path: &str) -> Result<RegKey, String> {
        let path = wide(path);
        let mut key = HKEY::default();
        // SAFETY: `parent` is a predefined or open key, the path is
        // NUL-terminated, and `key` receives the handle.
        let status = unsafe { RegOpenKeyExW(parent, PCWSTR(path.as_ptr()), None, KEY_READ, &mut key) };
        check(status, "RegOpenKeyExW")?;
        Ok(RegKey(key))
    }

    impl Drop for RegKey {
        fn drop(&mut self) {
            // SAFETY: the handle was opened by RegOpenKeyExW and is closed once.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- tests/test_system_identity.py: MicrocodeTest
    // Windows packs the revision into the high half of a QWORD.

    #[test]
    fn a_packed_qword_gives_the_upper_half() {
        assert_eq!(decode_microcode(&RegValue::Int(0x13300000000)).as_deref(), Some("0x133"));
    }

    #[test]
    fn a_plain_revision_is_left_alone() {
        assert_eq!(decode_microcode(&RegValue::Int(0xB404035)).as_deref(), Some("0xB404035"));
    }

    #[test]
    fn a_qword_with_an_empty_upper_half_falls_back_to_the_lower() {
        assert_eq!(
            decode_microcode(&RegValue::Int(0x1_0000_B404035 & 0xFFFFFFFF)).as_deref(),
            Some("0xB404035")
        );
    }

    #[test]
    fn bytes_are_read_little_endian() {
        assert_eq!(
            decode_microcode(&RegValue::Bytes(0x133u64.to_le_bytes().to_vec())).as_deref(),
            Some("0x133")
        );
    }

    #[test]
    fn anything_else_reads_as_nothing() {
        // A row that cannot be read shows an em dash; it does not invent one.
        assert_eq!(decode_microcode(&RegValue::Text("0x133".into())), None);
        assert_eq!(decode_microcode(&RegValue::None), None);
    }

    #[test]
    fn an_unreadable_registry_is_not_an_error() {
        // Under test every registry open fails, as the patched OpenKey did.
        assert_eq!(microcode(), None);
    }

    // --- MemoryTypeTest

    fn smbios(code: i64) -> Option<&'static str> {
        SMBIOS_MEMORY_TYPES.iter().find(|(known, _)| *known == code).map(|(_, name)| *name)
    }

    #[test]
    fn the_ddr5_code_is_decoded() {
        assert_eq!(smbios(0x22), Some("DDR5"));
    }

    #[test]
    fn ddr4_and_ddr3_are_decoded_too() {
        assert_eq!(smbios(0x1A), Some("DDR4"));
        assert_eq!(smbios(0x18), Some("DDR3"));
    }

    // --- LpcioTest: the vendor comes from the reader that answered, not the
    // chip name. `lpcio_from` takes what `detected_lpcio_chip` returns, which
    // is the reader's module and its chip name.

    fn chip(module: &'static str, name: &str) -> Option<(&'static str, String)> {
        Some((module, name.to_string()))
    }

    #[test]
    fn a_nuvoton_reader_names_nuvoton() {
        assert_eq!(lpcio_from(chip("superio_lpc", "NCT6687D")).as_deref(), Some("Nuvoton NCT6687D"));
    }

    #[test]
    fn an_ite_reader_names_ite() {
        assert_eq!(lpcio_from(chip("ite_superio", "IT8696E")).as_deref(), Some("ITE IT8696E"));
    }

    #[test]
    fn an_unknown_reader_still_names_the_chip() {
        assert_eq!(lpcio_from(chip("other", "X1234")).as_deref(), Some("X1234"));
    }

    #[test]
    fn no_profile_reads_as_nothing() {
        assert_eq!(lpcio_from(None), None);
    }

    #[test]
    fn a_profile_with_no_chip_reads_as_nothing() {
        assert_eq!(lpcio_from(chip("superio_lpc", "")), None);
    }

    // --- tests/test_system_info_additions.py: PcieLinkReadTest

    type Functions = BTreeMap<PciLocation, BTreeMap<u32, u32>>;

    /// A fake pci_config_dword over `{(bus, dev, fn): {offset: dword}}`.
    fn config_space(functions: Functions) {
        set_thread_config_space(Some(Box::new(move |device, function, offset, bus| {
            functions.get(&(bus, device, function)).and_then(|f| f.get(&offset)).copied()
        })));
    }

    /// A function whose only capability is PCI Express at 0x40.
    fn express_function(port_type: u32, link_cap: u32, link_status: u32) -> BTreeMap<u32, u32> {
        BTreeMap::from([
            (0x34, 0x40),
            (0x40, PCI_EXPRESS_CAPABILITY | (port_type << 20)),
            (0x4C, link_cap),
            (0x50, link_status << 16),
        ])
    }

    fn speed_width(generation: u32, width: u32) -> u32 {
        generation | (width << 4)
    }

    const ROOT: PciLocation = (0, 1, 1);
    const UPSTREAM: PciLocation = (1, 0, 0);
    const GPU: PciLocation = (3, 0, 0);

    fn read_link(functions: Functions, path: Option<&[PciLocation]>) -> Option<PcieLink> {
        config_space(functions);
        let link = pcie_link(Some(path.unwrap_or(&[ROOT, UPSTREAM, GPU])));
        set_thread_config_space(None);
        link
    }

    #[test]
    fn the_link_is_read_at_the_root_port() {
        // As on the bench: the root port and the card's upstream switch port
        // agree on Gen 5 x16 capable, running x2; the GPU behind the switch
        // would say x16, which is the switch's internal link.
        let link = read_link(
            BTreeMap::from([
                (ROOT, express_function(4, speed_width(5, 16), speed_width(5, 2))),
                (UPSTREAM, express_function(5, speed_width(5, 16), speed_width(5, 2))),
                (GPU, express_function(0, speed_width(5, 16), speed_width(5, 16))),
            ]),
            None,
        );
        assert_eq!(link, Some(PcieLink { max_gen: 5, max_width: 16, generation: 5, width: 2 }));
    }

    #[test]
    fn the_best_is_the_lower_of_the_two_ends() {
        // A Gen 4 card in a Gen 5 slot can train to Gen 4 at most.
        let link = read_link(
            BTreeMap::from([
                (ROOT, express_function(4, speed_width(5, 16), speed_width(4, 16))),
                (UPSTREAM, express_function(5, speed_width(4, 16), speed_width(4, 16))),
            ]),
            None,
        )
        .unwrap();
        assert_eq!((link.max_gen, link.max_width), (4, 16));
    }

    #[test]
    fn a_path_that_does_not_start_at_a_root_port_is_refused() {
        let link = read_link(
            BTreeMap::from([
                (ROOT, express_function(6, speed_width(5, 16), speed_width(5, 16))),
                (UPSTREAM, express_function(5, speed_width(5, 16), speed_width(5, 16))),
            ]),
            None,
        );
        assert_eq!(link, None);
    }

    #[test]
    fn missing_config_space_is_nothing_rather_than_zero() {
        assert_eq!(read_link(BTreeMap::new(), None), None);
        assert_eq!(read_link(BTreeMap::new(), Some(&[GPU])), None);
    }

    // --- Beyond the Python tests: the pure decoders the profiles lean on.

    #[test]
    fn the_processor_signature_decodes_family_model_and_stepping() {
        // An i9-14900K's leaf-1 EAX, 0xB0671: family 6, model 0xB7, stepping 1.
        assert_eq!(decode_wmi_processor_signature(Some("BFEBFBFF000B0671")), Ok((6, 0xB7, 1)));
        // A Zen 5 desktop part, 0xB40F40: family 0x1A, model 0x44.
        assert_eq!(decode_wmi_processor_id(Some("178BFBFF00B40F40")), Ok((0x1A, 0x44)));
        assert_eq!(
            decode_wmi_processor_signature(Some("1234")),
            Err("WMI ProcessorId is unavailable".to_string())
        );
        assert_eq!(
            decode_wmi_processor_signature(None),
            Err("WMI ProcessorId is unavailable".to_string())
        );
        assert_eq!(
            decode_wmi_processor_signature(Some("ZZZZZZZZZZ")),
            Err("WMI ProcessorId is invalid".to_string())
        );
    }

    #[test]
    fn the_adapter_identity_comes_from_the_pnp_id() {
        let ids = vec![
            r"ROOT\BASICDISPLAY".to_string(),
            r"PCI\VEN_1002&DEV_7550&SUBSYS_2426148C&REV_C0\6&1A2B3C4D&0&00000019".to_string(),
        ];
        assert_eq!(
            adapter_identity(Some(&ids)),
            Some(AdapterIdentity {
                vendor_id: 0x1002,
                device_id: 0x7550,
                subsystem_vendor_id: 0x148C,
                subsystem_device_id: 0x2426,
                revision: 0xC0,
            })
        );
        assert_eq!(adapter_identity(Some(&[])), None);
        assert_eq!(board_vendor(Some(0x148C)).as_deref(), Some("PowerColor"));
        assert_eq!(board_vendor(Some(0x1234)).as_deref(), Some("0x1234"));
        assert_eq!(board_vendor(None), None);
    }

    #[test]
    fn the_memory_type_names_each_generation_once() {
        use crate::wmi_util::{FakeWmi, WmiObject};
        FakeWmi::new()
            .class(
                "Win32_PhysicalMemory",
                vec![
                    WmiObject::new().with("SMBIOSMemoryType", 0x22u32),
                    WmiObject::new().with("SMBIOSMemoryType", 0x22u32),
                    WmiObject::new().with("SMBIOSMemoryType", 0x30u32),
                ],
            )
            .install();
        assert_eq!(memory_type().as_deref(), Some("DDR5 / Type 48"));
        crate::wmi_util::set_thread_source(None);
    }

    #[test]
    fn registry_values_convert_as_winreg_does() {
        use windows::Win32::System::Registry::{REG_BINARY, REG_DWORD, REG_MULTI_SZ, REG_SZ};
        assert_eq!(RegValue::from_raw(REG_DWORD, &[0x2F, 0x18, 0, 0]), RegValue::Int(0x182F));
        let text: Vec<u8> = "23H2\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(RegValue::from_raw(REG_SZ, &text), RegValue::Text("23H2".into()));
        let multi: Vec<u8> = "a\0b\0\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(
            RegValue::from_raw(REG_MULTI_SZ, &multi),
            RegValue::Multi(vec!["a".into(), "b".into()])
        );
        assert_eq!(RegValue::from_raw(REG_BINARY, &[]), RegValue::None);
        assert_eq!(RegValue::Bytes(vec![0x41, 0x00, 0x27]).py_str(), "b\"A\\x00'\"");
    }
}
