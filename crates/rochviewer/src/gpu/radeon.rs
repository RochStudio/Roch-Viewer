//! What can be read about an AMD Radeon, and what cannot.
//!
//! Four sources, none of them a vendor SDK: the PNP device id Windows already
//! holds, which carries the four PCI identifiers; the display class registry key,
//! which carries the adapter string and the memory size; the card's own PCIe
//! Resizable BAR capability, read out of configuration space; and a table below
//! that turns a device id into the names AMD publishes for that silicon.
//!
//! The table is a decode table, not a transcript. "Navi 48" is a name for the
//! code 0x7550 read from the card, the same way "SK hynix" is a name for a JEDEC
//! vendor code. What it is *not* is a substitute for reading: an unlisted device
//! gets no rows at all rather than a neighbouring part's numbers, because a table
//! that guesses is one that is wrong quietly.
//!
//! Memory vendor and type come from a fifth source, AMD's own driver library --
//! see amd_adl, and see the Intel profile doing the same thing through NVAPI.
//! Nothing readable as a register carries the vendor: the expansion ROM BAR is
//! unassigned so there is no VBIOS to parse, MC_SEQ_MISC0 answers all-ones on
//! RDNA4, and the driver's registry subtree has no vendor string in it. The
//! library has it because the driver read the VBIOS at load time.

use std::collections::BTreeMap;

use crate::amd::adl::read_memory;
use crate::pyfmt;
use crate::system_identity::registry::RegKey;
use crate::system_identity::{
    PcieLink, RegValue, adapter_identity, board_vendor, find_display_path, pcie_link, py_int,
    py_strip, resizable_bar_megabytes,
};
use crate::wmi_util;

pub const AMD_VENDOR_ID: u16 = 0x1002;

pub const DISPLAY_CLASS_KEY: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";

/// What AMD publishes for one cut of a silicon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Silicon {
    pub code_name: &'static str,
    pub technology: &'static str,
    pub cores: u32,
    pub rops: u32,
    pub tmus: u32,
    pub memory_type: &'static str,
    pub bus_width: &'static str,
}

/// Per (device id, revision): the silicon's name, its process node, and the
/// shader, ROP and TM unit counts AMD publishes for the part.
///
/// Keyed on the revision as well as the device, because one device id covers a
/// family: 0x7550 is Navi 48, and the cut a board carries is what decides the
/// unit counts. Only the pair measured on this bench is listed. Every other
/// card reads an em dash, which is the honest answer until one is in front of
/// the tool.
///
/// Bench card: PowerColor Radeon RX 9070 XT, DEV_7550 REV_C0, reporting
/// 17,095,983,104 bytes of memory through the display class key.
pub const GPU_SILICON: &[((u16, u8), Silicon)] = &[(
    (0x7550, 0xC0),
    Silicon {
        code_name: "Navi 48",
        technology: "4 nm",
        cores: 4096,
        rops: 128,
        tmus: 256,
        memory_type: "GDDR6",
        bus_width: "256 bit",
    },
)];

/// Below this the BAR is the 256 MB legacy window every card has had since
/// PCI. Anything larger means the firmware sized it to the card's memory,
/// which is what Resizable BAR being on means.
pub const LEGACY_BAR_MEGABYTES: u64 = 256;

/// Everything known about the installed Radeon. Every field but the PCI
/// identity is optional: a field that could not be read is absent rather
/// than blank, so the caller shows an em dash rather than a zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RadeonGpu {
    pub device_id: u16,
    pub revision: u8,
    /// GPU-Z spells the revision as the bare hex byte the card reports.
    pub revision_text: String,
    pub board_manufacturer: Option<String>,
    pub name: Option<String>,
    pub vbios: Option<String>,
    pub memory_size: Option<String>,
    pub code_name: Option<String>,
    pub technology: Option<String>,
    pub cores: Option<u32>,
    pub rops: Option<u32>,
    pub tmus: Option<u32>,
    pub memory_type: Option<String>,
    pub bus_width: Option<String>,
    pub resizable_bar: Option<String>,
    pub pcie_link: Option<String>,
    pub driver_version: Option<String>,
    pub driver_date: Option<String>,
    pub memory_vendor: Option<String>,
    pub memory_bytes: Option<i64>,
}

impl RadeonGpu {
    /// One field by its Python key, as `str(found.get(key))` -- `None` when
    /// the field is absent or falsy (an empty string or a zero), which is
    /// when the profile's `found.get(key) or EM_DASH` showed the dash.
    pub fn text(&self, key: &str) -> Option<String> {
        let number = |value: Option<i64>| value.filter(|v| *v != 0).map(|v| v.to_string());
        let text = |value: &Option<String>| value.clone().filter(|v| !v.is_empty());
        match key {
            "device_id" => number(Some(self.device_id as i64)),
            "revision" => number(Some(self.revision as i64)),
            "revision_text" => Some(self.revision_text.clone()).filter(|v| !v.is_empty()),
            "board_manufacturer" => text(&self.board_manufacturer),
            "name" => text(&self.name),
            "vbios" => text(&self.vbios),
            "memory_size" => text(&self.memory_size),
            "code_name" => text(&self.code_name),
            "technology" => text(&self.technology),
            "cores" => number(self.cores.map(i64::from)),
            "rops" => number(self.rops.map(i64::from)),
            "tmus" => number(self.tmus.map(i64::from)),
            "memory_type" => text(&self.memory_type),
            "bus_width" => text(&self.bus_width),
            "resizable_bar" => text(&self.resizable_bar),
            "pcie_link" => text(&self.pcie_link),
            "driver_version" => text(&self.driver_version),
            "driver_date" => text(&self.driver_date),
            "memory_vendor" => text(&self.memory_vendor),
            "memory_bytes" => number(self.memory_bytes),
            _ => None,
        }
    }
}

/// The display class key's values for the first real adapter.
fn registry_adapter() -> BTreeMap<String, RegValue> {
    read_registry_adapter().unwrap_or_default()
}

fn read_registry_adapter() -> Result<BTreeMap<String, RegValue>, String> {
    let parent = RegKey::open_local_machine(DISPLAY_CLASS_KEY)?;
    for index in 0..16 {
        let Ok(name) = parent.enum_key(index) else {
            break;
        };
        // str.isdigit(): every character a digit, and at least one of them.
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let key = parent.open(&name)?;
        let mut values = BTreeMap::new();
        for slot in 0..key.value_count()? {
            let (value_name, value) = key.enum_value(slot)?;
            values.insert(value_name, value);
        }
        if values.get("HardwareInformation.qwMemorySize").is_some_and(RegValue::is_truthy) {
            return Ok(values);
        }
    }
    Ok(BTreeMap::new())
}

/// Render the adapter's memory the way a card is sold.
///
/// Rounded to whole gigabytes because the firmware carves a little off what
/// it reports: this card answers 17,095,983,104 bytes, which is 80 MiB under
/// a round 16 GiB. Printing 15.92 would be precise about the wrong thing.
pub fn memory_size_text(byte_count: u64) -> Option<String> {
    if byte_count == 0 {
        return None;
    }
    Some(gigabytes_text(byte_count))
}

/// `"%d GB" % round(int(byte_count) / (1024 ** 3))`.
fn gigabytes_text(byte_count: u64) -> String {
    format!("{} GB", pyfmt::round_int(byte_count as f64 / 1073741824.0))
}

/// Python's `int(value)` on a registry value: a number as itself, text and
/// bytes parsed as decimal digits. What int() refused raised, and the whole
/// card read failed with it.
fn registry_int(value: &RegValue) -> Result<u64, String> {
    let parsed = match value {
        RegValue::Int(number) => return Ok(*number),
        RegValue::Text(text) => py_int(text, 10),
        RegValue::Bytes(bytes) => std::str::from_utf8(bytes).ok().and_then(|text| py_int(text, 10)),
        _ => None,
    };
    parsed
        .and_then(|number| u64::try_from(number).ok())
        .ok_or_else(|| format!("invalid literal for int(): {}", value.py_str()))
}

/// Render the BAR size as the verdict plus the reading behind it.
pub fn resizable_bar_text(megabytes: Option<u64>) -> Option<String> {
    let megabytes = megabytes.filter(|mb| *mb != 0)?;
    let state = if megabytes > LEGACY_BAR_MEGABYTES { "Enabled" } else { "Disabled" };
    if megabytes >= 1024 && megabytes % 1024 == 0 {
        return Some(format!("{state} ({} GB)", megabytes / 1024));
    }
    Some(format!("{state} ({megabytes} MB)"))
}

/// GPU-Z's names for the link speeds: 2.5 GT/s is "1.1", not "1.0".
pub const PCIE_GENERATIONS: &[(u32, &str)] =
    &[(1, "1.1"), (2, "2.0"), (3, "3.0"), (4, "4.0"), (5, "5.0"), (6, "6.0")];

fn generation_name(code: u32) -> Option<&'static str> {
    PCIE_GENERATIONS.iter().find(|(known, _)| *known == code).map(|(_, name)| *name)
}

/// "PCIe 5.0 x16", with how it runs now after an "@" when that differs.
///
/// The capability first, as the Intel profile's NVIDIA row writes it, then
/// GPU-Z's "@" for the live state: a Radeon narrows and slows its link at
/// idle to save power, so "PCIe 5.0 x16 @ 5.0 x2" is a card resting in a
/// full slot, not a card in the wrong one.
pub fn pcie_link_text(link: Option<&PcieLink>) -> Option<String> {
    let link = link?;
    let best = generation_name(link.max_gen)?;
    let now = generation_name(link.generation)?;
    let mut text = format!("PCIe {best} x{}", link.max_width);
    if (link.generation, link.width) != (link.max_gen, link.max_width) {
        text += &format!(" @ {now} x{}", link.width);
    }
    Some(text)
}

fn registry_text(value: Option<&RegValue>) -> String {
    let text = match value {
        Some(RegValue::Bytes(bytes)) => {
            // UTF-16LE, dropping what does not decode, as "ignore" did.
            let units: Vec<u16> =
                bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
            char::decode_utf16(units).filter_map(Result::ok).collect()
        }
        Some(value) if value.is_truthy() => value.py_str(),
        _ => String::new(),
    };
    py_strip(text.trim_matches('\0')).to_string()
}

/// WMI datetime yyyymmddHHMMSS -> yyyy-mm-dd, as the BIOS date reads.
pub fn driver_date_text(raw: Option<&str>) -> Option<String> {
    let raw: Vec<char> = raw.unwrap_or("").chars().collect();
    if raw.len() < 8 || !raw[..8].iter().all(char::is_ascii_digit) {
        return None;
    }
    let part = |range: std::ops::Range<usize>| raw[range].iter().collect::<String>();
    Some(format!("{}-{}-{}", part(0..4), part(4..6), part(6..8)))
}

/// What is known about the installed Radeon: `Ok(None)` when there is no
/// AMD card, which the Python returned as an empty dict.
///
/// An `Err` is the Python raising: a display class key whose memory size is
/// not a number. The profile's getter caught that and showed a dash without
/// caching anything.
pub fn read_gpu(pnp_device_ids: Option<&[String]>) -> Result<Option<RadeonGpu>, String> {
    let Some(identity) = adapter_identity(pnp_device_ids) else {
        return Ok(None);
    };
    if identity.vendor_id != AMD_VENDOR_ID {
        return Ok(None);
    }

    let mut found = RadeonGpu {
        device_id: identity.device_id,
        revision: identity.revision,
        // GPU-Z spells the revision as the bare hex byte the card reports.
        revision_text: format!("{:02X}", identity.revision),
        ..RadeonGpu::default()
    };
    found.board_manufacturer = board_vendor(Some(identity.subsystem_vendor_id));

    let registry = registry_adapter();
    let name = registry_text(registry.get("HardwareInformation.AdapterString"));
    if !name.is_empty() {
        found.name = Some(name);
    }
    // The VBIOS part number the driver read at load, "113-EXT114484-100":
    // what AMD Software lists as the VBIOS part number. There is no ROM to
    // read it from ourselves -- see the module docs.
    let vbios = registry_text(registry.get("HardwareInformation.BiosString"));
    if !vbios.is_empty() {
        found.vbios = Some(vbios);
    }
    if let Some(value) = registry.get("HardwareInformation.qwMemorySize").filter(|v| v.is_truthy()) {
        // The emptiness test is on the value as stored, before int(): a
        // stored "0" is not empty, and reads as "0 GB".
        found.memory_size = Some(gigabytes_text(registry_int(value)?));
    }

    if let Some((_, silicon)) =
        GPU_SILICON.iter().find(|(key, _)| *key == (identity.device_id, identity.revision))
    {
        found.code_name = Some(silicon.code_name.into());
        found.technology = Some(silicon.technology.into());
        found.cores = Some(silicon.cores);
        found.rops = Some(silicon.rops);
        found.tmus = Some(silicon.tmus);
        found.memory_type = Some(silicon.memory_type.into());
        found.bus_width = Some(silicon.bus_width.into());
    }

    let path = find_display_path(AMD_VENDOR_ID, 0, 0);
    let location = path.as_ref().and_then(|path| path.last().copied());
    if let Some(bar) = resizable_bar_text(resizable_bar_megabytes(location)) {
        found.resizable_bar = Some(bar);
    }
    if let Some(link) = pcie_link_text(pcie_link(path.as_deref()).as_ref()) {
        found.pcie_link = Some(link);
    }

    let (driver_version, driver_date) = driver_details();
    if driver_version.is_some() {
        found.driver_version = driver_version;
    }
    if driver_date.is_some() {
        found.driver_date = driver_date;
    }

    // Last, so the driver's own answers win over the table's. It reports the
    // memory type as well, which is the same GDDR6 the table claims -- read
    // rather than looked up is better on principle, and it means a card the
    // table does not know still gets a type.
    let memory = read_memory(location);
    if memory.memory_type.is_some() {
        found.memory_type = memory.memory_type;
    }
    if memory.memory_vendor.is_some() {
        found.memory_vendor = memory.memory_vendor;
    }
    if memory.memory_bytes.is_some() {
        found.memory_bytes = memory.memory_bytes;
    }
    Ok(Some(found))
}

/// The installed driver's version and release date, from WMI.
fn driver_details() -> (Option<String>, Option<String>) {
    let Ok(adapters) = wmi_util::query("Win32_VideoController") else {
        return (None, None);
    };
    for adapter in &adapters {
        if adapter.str_or_empty("Name").contains("Basic Display") {
            continue;
        }
        let version = adapter.str_or_empty("DriverVersion");
        let version = py_strip(&version);
        let version = if version.is_empty() { None } else { Some(version.to_string()) };
        let date = driver_date_text(adapter.text("DriverDate").as_deref());
        return (version, date);
    }
    (None, None)
}

/// ROP and TM unit counts as one row, the way GPU-Z pairs them.
pub fn rops_tmus_text(found: Option<&RadeonGpu>) -> Option<String> {
    let found = found?;
    let (rops, tmus) = (found.rops?, found.tmus?);
    Some(format!("{rops} / {tmus}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- tests/test_system_info_additions.py: PcieLinkTextTest

    fn link(max_gen: u32, max_width: u32, generation: u32, width: u32) -> PcieLink {
        PcieLink { max_gen, max_width, generation, width }
    }

    #[test]
    fn a_link_at_its_best_is_one_figure() {
        assert_eq!(pcie_link_text(Some(&link(5, 16, 5, 16))).as_deref(), Some("PCIe 5.0 x16"));
    }

    #[test]
    fn a_resting_link_shows_how_it_runs_now() {
        // The bench card at idle: a Gen 5 x16 slot running Gen 5 x2.
        assert_eq!(
            pcie_link_text(Some(&link(5, 16, 5, 2))).as_deref(),
            Some("PCIe 5.0 x16 @ 5.0 x2")
        );
        assert_eq!(
            pcie_link_text(Some(&link(4, 16, 1, 16))).as_deref(),
            Some("PCIe 4.0 x16 @ 1.1 x16")
        );
    }

    #[test]
    fn nothing_read_is_nothing_shown() {
        assert_eq!(pcie_link_text(None), None);
        assert_eq!(pcie_link_text(Some(&link(9, 16, 5, 16))), None);
    }

    // --- RadeonRegistryTextTest

    #[test]
    fn the_bios_string_decodes_from_utf16() {
        let raw: Vec<u8> =
            "113-EXT114484-100\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(registry_text(Some(&RegValue::Bytes(raw))), "113-EXT114484-100");
        assert_eq!(registry_text(None), "");
    }

    // --- Beyond the Python tests: the pure formatters read_gpu is built from.

    #[test]
    fn the_formatters_render_as_gpu_z_does() {
        // The bench card's 17,095,983,104 bytes are a 16 GB card.
        assert_eq!(memory_size_text(17_095_983_104).as_deref(), Some("16 GB"));
        assert_eq!(memory_size_text(0), None);
        assert_eq!(resizable_bar_text(Some(16384)).as_deref(), Some("Enabled (16 GB)"));
        assert_eq!(resizable_bar_text(Some(256)).as_deref(), Some("Disabled (256 MB)"));
        assert_eq!(resizable_bar_text(Some(0)), None);
        assert_eq!(
            driver_date_text(Some("20250312000000.000000-000")).as_deref(),
            Some("2025-03-12")
        );
        assert_eq!(driver_date_text(Some("2025")), None);
        assert_eq!(registry_int(&RegValue::Text(" 1_024 ".into())), Ok(1024));
        assert!(registry_int(&RegValue::Bytes(vec![0, 0, 0, 4])).is_err());
    }

    #[test]
    fn another_vendors_card_is_no_radeon() {
        let ids = vec![r"PCI\VEN_10DE&DEV_2786&SUBSYS_40C61458&REV_A1\4&0".to_string()];
        assert_eq!(read_gpu(Some(&ids)), Ok(None));
        assert_eq!(read_gpu(Some(&[])), Ok(None));
    }

    #[test]
    fn the_bench_card_is_named_from_the_table_and_rops_pair_with_tmus() {
        // No registry, configuration space, WMI or ADL under test: what is left
        // is the PNP identity and the decode table.
        let ids = vec![r"PCI\VEN_1002&DEV_7550&SUBSYS_2426148C&REV_C0\6&0".to_string()];
        let found = read_gpu(Some(&ids)).unwrap().unwrap();
        assert_eq!(found.revision_text, "C0");
        assert_eq!(found.board_manufacturer.as_deref(), Some("PowerColor"));
        assert_eq!(found.code_name.as_deref(), Some("Navi 48"));
        assert_eq!(found.text("cores").as_deref(), Some("4096"));
        assert_eq!(found.text("resizable_bar"), None);
        assert_eq!(rops_tmus_text(Some(&found)).as_deref(), Some("128 / 256"));
        assert_eq!(rops_tmus_text(Some(&RadeonGpu::default())), None);
        assert_eq!(rops_tmus_text(None), None);
    }
}
