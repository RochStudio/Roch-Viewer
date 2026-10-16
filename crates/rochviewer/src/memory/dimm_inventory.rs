//! Per-DIMM SMBIOS facts, shared by the bottom DIMM strip and System Info.
//!
//! Both readers want the same four things about an installed module -- part
//! number, capacity, rank count and DRAM component -- and both used to derive
//! them from `Win32_PhysicalMemory` on their own. The query lives here once so
//! the two displays cannot drift apart, and so the decode can be tested
//! without WMI by installing a stand-in source.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use super::ddr5_spd::SpdIdentity;
use super::dram_ic::identify_dram_ic;
use crate::memo::Memo;
use crate::wmi_util::{self, WmiObject, WmiValue};

pub const EM_DASH: &str = "—";

// SMBIOS DeviceLocator is the name the board itself gives the socket. Three
// shapes cover what desktop firmware writes:
//
//   "Controller0-DIMMA2", "DIMM_A1", "DIMMB2"    channel letter then number
//   "Controller0-ChannelA-DIMM1"                 channel letter, index in it
//   "Controller0-DIMM0"                          controller index, index in it
//
// Reading it beats deriving a slot from the record's position in the query.
// Position is an SMBIOS handle order, not a slot order: on the MSI Z790-P the
// two installed modules come back as records 1 and 3 while the board calls them
// DIMMA2 and DIMMB2, so a position-based table got the slot numbers wrong and
// the channels backwards.
//
// The third shape is weaker than the other two and is tried last. It names no
// channel letter, so the letter is the controller index (0 -> A) and the number
// is the DIMM index within that controller counted from one. That is a derived
// name rather than the board's own: firmware writing this shape never states
// the silkscreen number, so on a board whose two sockets are labelled A2/B2 the
// derived name reads A1/B1. It is still tied to the record's own controller and
// DIMM index rather than its position in the query, so the channel it names is
// right and the two sticks cannot swap -- which is the failure that mattered.
// ASUS ROG MAXIMUS Z790 APEX (LGA1700 DDR5) writes this shape.
static SLOT_LETTER_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)DIMM[\s_-]*([A-Za-z])(\d+)").unwrap());
static SLOT_CHANNEL_INDEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)CHANNEL[\s_-]*([A-Za-z]).*?DIMM[\s_-]*(\d+)").unwrap());
static SLOT_CONTROLLER_INDEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)CONTROLLER[\s_-]*(\d+).*?DIMM[\s_-]*(\d+)").unwrap());
static BANK_CHANNEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^P\d+\s+CHANNEL\s+([A-Z])$").unwrap());
static BANK_DIMM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^DIMM\s+(\d+)$").unwrap());

fn number(digits: &str) -> Option<u64> {
    digits.parse().ok()
}

/// The board's name for a slot, like "A2", or `None` when unreadable.
///
/// `None` rather than a guess: a wrong slot name points a tuner at the wrong
/// stick, which is worse than admitting the board did not say. A locator or
/// bank label that is absent is passed as "".
pub fn parse_slot(device_locator: &str, bank_label: &str) -> Option<String> {
    let text = device_locator;
    // Gigabyte AM5 separates the channel and socket into BankLabel and
    // DeviceLocator. Preserve the firmware's DIMM number, not query order.
    let bank_channel = BANK_CHANNEL.captures(bank_label.trim());
    let bank_dimm = BANK_DIMM.captures(text.trim());
    if let (Some(channel), Some(dimm)) = (bank_channel, bank_dimm) {
        return Some(format!("{}{}", channel[1].to_uppercase(), number(&dimm[1])?));
    }
    if let Some(found) = SLOT_LETTER_NUMBER.captures(text) {
        return Some(format!("{}{}", found[1].to_uppercase(), number(&found[2])?));
    }
    if let Some(found) = SLOT_CHANNEL_INDEX.captures(text) {
        // This form numbers the DIMMs within a channel from zero.
        return Some(format!("{}{}", found[1].to_uppercase(), number(&found[2])? + 1));
    }
    if let Some(found) = SLOT_CONTROLLER_INDEX.captures(text) {
        let controller = number(&found[1])?;
        // Past Z the letter would run into punctuation; no desktop board has
        // anywhere near that many controllers, so decline rather than invent.
        if controller < 26 {
            return Some(format!("{}{}", (b'A' + controller as u8) as char, number(&found[2])? + 1));
        }
    }
    None
}

/// The channel letter a slot name belongs to, or `None`.
pub fn channel_of(slot: Option<&str>) -> Option<String> {
    slot.and_then(|slot| slot.chars().next()).map(String::from)
}

/// Derive a slot from controller/DIMM indexes when firmware labels collide.
fn controller_slot(device_locator: &str) -> Option<String> {
    let found = SLOT_CONTROLLER_INDEX.captures(device_locator)?;
    let controller = number(&found[1])?;
    if controller >= 26 {
        return None;
    }
    Some(format!("{}{}", (b'A' + controller as u8) as char, number(&found[2])? + 1))
}

/// Use controller indexes to disambiguate impossible duplicate slots.
///
/// Some ASUS four-DIMM firmware restarts the channel letter inside each
/// controller. Its populated sockets therefore arrive as
/// `Controller0-ChannelA-DIMM1` and `Controller1-ChannelA-DIMM1`. Parsed
/// literally both are A2, even though the controller numbers identify the
/// physical channels as A and B. Keep normal board labels authoritative and
/// apply the controller fallback only when the parsed names collide and the
/// fallback produces a complete, unique set.
fn resolve_duplicate_slots(modules: &mut [DimmModule]) {
    // Slot -> module indexes, in first-seen order of the slot.
    let mut by_slot: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, module) in modules.iter().enumerate() {
        let Some(slot) = module.slot.clone().filter(|s| !s.is_empty()) else {
            continue;
        };
        match by_slot.iter_mut().find(|(known, _)| *known == slot) {
            Some((_, indexes)) => indexes.push(index),
            None => by_slot.push((slot, vec![index])),
        }
    }

    for (_, duplicates) in by_slot {
        if duplicates.len() < 2 {
            continue;
        }
        let resolved: Vec<Option<String>> =
            duplicates.iter().map(|&i| controller_slot(&modules[i].device_locator)).collect();
        if resolved.iter().any(Option::is_none) {
            continue;
        }
        let unique: std::collections::BTreeSet<&Option<String>> = resolved.iter().collect();
        if unique.len() != resolved.len() {
            continue;
        }
        for (index, slot) in duplicates.into_iter().zip(resolved) {
            modules[index].channel = channel_of(slot.as_deref());
            modules[index].slot = slot;
        }
    }
}

/// Anything with a slot: `{channel letter: [slot names]}` groups these.
pub trait HasSlot {
    fn slot(&self) -> Option<&str>;
}

impl HasSlot for DimmModule {
    fn slot(&self) -> Option<&str> {
        self.slot.as_deref()
    }
}

impl HasSlot for Option<String> {
    fn slot(&self) -> Option<&str> {
        self.as_deref()
    }
}

/// `{channel letter: [slot names]}` for the installed modules.
pub fn slots_by_channel<M: HasSlot>(modules: &[M]) -> BTreeMap<String, Vec<String>> {
    let mut channels: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for module in modules {
        let Some(slot) = module.slot().filter(|s| !s.is_empty()) else {
            continue;
        };
        if let Some(channel) = channel_of(Some(slot)) {
            channels.entry(channel).or_default().push(slot.to_string());
        }
    }
    for slots in channels.values_mut() {
        slots.sort();
    }
    channels
}

// Text fields worth handing to the IC identifier. SMBIOS usually exposes the
// module SKU but not the DRAM component marking, so anything a vendor happens
// to fill in is a chance at a real answer.
const METADATA_FIELDS: [&str; 8] =
    ["SerialNumber", "AssetTag", "DeviceLocator", "BankLabel", "Name", "Description", "Model", "SKU"];

static CACHE: Memo<Vec<DimmModule>> = Memo::new();

/// One installed module, as SMBIOS Type 17 describes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DimmModule {
    pub serial_number: String,
    pub tag: String,
    pub device_locator: String,
    pub slot: Option<String>,
    pub channel: Option<String>,
    pub part_number: String,
    pub capacity_gb: i64,
    pub capacity: String,
    pub rank_count: u32,
    /// 1R / 2R, as ZenTimings and the rest of this viewer write it.
    /// rank_short's SR / DR stays the DRAM IC lookup's own key.
    pub rank: String,
    pub module_manufacturer: String,
    pub ic: String,
}

impl DimmModule {
    /// A field by its Python dict key, as text (`module.get(field)`).
    pub fn get(&self, field: &str) -> Option<String> {
        match field {
            "serial_number" => Some(self.serial_number.clone()),
            "tag" => Some(self.tag.clone()),
            "device_locator" => Some(self.device_locator.clone()),
            "slot" => self.slot.clone(),
            "channel" => self.channel.clone(),
            "part_number" => Some(self.part_number.clone()),
            "capacity_gb" => Some(self.capacity_gb.to_string()),
            "capacity" => Some(self.capacity.clone()),
            "rank_count" => Some(self.rank_count.to_string()),
            "rank" => Some(self.rank.clone()),
            "module_manufacturer" => Some(self.module_manufacturer.clone()),
            "ic" => Some(self.ic.clone()),
            _ => None,
        }
    }
}

/// Python truthiness for a WMI value.
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

/// The rank count from SMBIOS Type 17 Attributes, 0 when absent.
///
/// The low nibble carries the rank count; firmware that does not fill the
/// field leaves it zero.
pub fn rank_count(attributes: Option<&WmiValue>) -> u32 {
    match attributes.filter(|value| truthy(value)) {
        None => 0,
        Some(value) => value.to_i64().map(|count| (count & 0x0F) as u32).unwrap_or(0),
    }
}

/// Rank as the DIMM strip writes it: SR / DR / 4R.
pub fn rank_short(count: u32) -> String {
    match count {
        1 => "SR".to_string(),
        2 => "DR".to_string(),
        c if c > 2 => format!("{c}R"),
        _ => "N/A".to_string(),
    }
}

/// Rank as ZenTimings writes it: 1R / 2R.
pub fn rank_numeric(count: u32) -> String {
    if count > 0 { format!("{count}R") } else { EM_DASH.to_string() }
}

/// Split an IC label into (manufacturer, die).
///
/// `identify_dram_ic` returns one string because that is what the DIMM strip
/// prints, but System Info lists the maker and the die separately.
///
/// ```text
///   "SK hynix A-die"        -> ("SK hynix", "A-die")
///   "Micron (die unknown)"  -> ("Micron", EM_DASH)
///   "SK hynix CJR"          -> ("SK hynix", "CJR")
///   "Unknown IC"            -> (EM_DASH, EM_DASH)
/// ```
pub fn split_ic(label: Option<&str>) -> (String, String) {
    let text = label.unwrap_or("").trim();
    if text.is_empty() || text == "Unknown IC" {
        return (EM_DASH.to_string(), EM_DASH.to_string());
    }
    if let Some(maker) = text.strip_suffix("(die unknown)") {
        let maker = maker.trim();
        return (if maker.is_empty() { EM_DASH } else { maker }.to_string(), EM_DASH.to_string());
    }
    match text.rsplit_once(' ') {
        Some((maker, die)) if !maker.is_empty() => (maker.to_string(), die.to_string()),
        _ => (text.to_string(), EM_DASH.to_string()),
    }
}

/// One record per installed module, or none when the query fails.
///
/// Cached: the DIMM strip and four System Info rows all read it during
/// startup, and each WMI connection costs far more than the values do.
pub fn read_modules(refresh: bool) -> Vec<DimmModule> {
    if !refresh && let Some(cached) = CACHE.get() {
        return cached;
    }
    let modules = query_modules();
    CACHE.set(modules.clone());
    modules
}

/// [`read_modules`] queried now and never cached: the Python's
/// `read_modules(connection)`.
pub fn query_modules() -> Vec<DimmModule> {
    match wmi_util::query("Win32_PhysicalMemory") {
        Ok(entries) => decode(&entries),
        Err(e) => {
            eprintln!("Error retrieving memory info: {e}");
            Vec::new()
        }
    }
}

/// Decode `Win32_PhysicalMemory` records.
pub fn decode(entries: &[WmiObject]) -> Vec<DimmModule> {
    let mut modules = Vec::new();
    for memory in entries {
        let capacity_gb = memory
            .get("Capacity")
            .filter(|value| truthy(value))
            .map(|value| value.to_i64().unwrap_or(0))
            .unwrap_or(0)
            .div_euclid(1024 * 1024 * 1024);
        let part_number = non_empty_or(memory.str_or_empty("PartNumber").trim(), "Unknown");
        let tag = non_empty_or(memory.str_or_empty("Tag").trim(), "Unknown");
        let module_manufacturer = memory.str_or_empty("Manufacturer").trim().to_string();
        let memory_type = ["SMBIOSMemoryType", "MemoryType"]
            .iter()
            .filter_map(|field| memory.get(field).filter(|value| truthy(value)))
            .find_map(WmiValue::to_text)
            .unwrap_or_default();

        let mut extra_metadata = Vec::new();
        for field_name in METADATA_FIELDS {
            if let Some(value) = memory.get(field_name).filter(|value| truthy(value))
                && let Some(text) = value.to_text()
            {
                extra_metadata.push(text);
            }
        }

        let ranks = rank_count(memory.get("Attributes"));
        let ic_label = identify_dram_ic(
            &part_number,
            &module_manufacturer,
            &memory_type,
            capacity_gb,
            &rank_short(ranks),
            &extra_metadata,
        );

        let device_locator = memory.str_or_empty("DeviceLocator").trim().to_string();
        let slot = parse_slot(&device_locator, &memory.str_or_empty("BankLabel"));

        // Firmware reads this off the module at POST. It is the one identity
        // field SMBIOS carries that no table can be derived from, and on DDR4
        // it is the only way to reach it: the SPD path that reads the serial
        // directly is DDR5-only. Firmware that leaves it blank, or fills it
        // with a placeholder of all zeroes or all Fs, is reporting nothing.
        let mut serial_number = memory.str_or_empty("SerialNumber").trim().to_string();
        if serial_number.trim_matches(['0', 'f', 'F']).is_empty() {
            serial_number = String::new();
        }

        modules.push(DimmModule {
            serial_number,
            tag,
            device_locator,
            channel: channel_of(slot.as_deref()),
            slot,
            part_number,
            capacity_gb,
            capacity: format!("{capacity_gb}GB"),
            rank_count: ranks,
            rank: rank_numeric(ranks),
            module_manufacturer,
            ic: ic_label,
        });
    }
    resolve_duplicate_slots(&mut modules);
    modules
}

fn non_empty_or(text: &str, fallback: &str) -> String {
    if text.is_empty() { fallback.to_string() } else { text.to_string() }
}

// Boards whose firmware overstates how many DIMM sockets they have, keyed on
// the board code inside Win32_BaseBoard.Product.
//
// SMBIOS Type 16 MemoryDevices is the only readable socket total, and a board
// that shares a firmware table with a larger sibling reports the sibling's
// count: MS-7E83 (MSI B850M POWER) is a two-slot board that declares four
// devices, listing DIMMA1 and DIMMB1 as empty sockets that do not exist.
//
// Nothing can be read to settle it -- an empty socket has no SPD hub to probe,
// and the controller only knows what is populated -- so a correction is
// entered per board from the board itself. Anything not listed keeps the
// firmware count, which is right on most boards.
pub const BOARD_MEMORY_SLOTS: &[(&str, i64)] = &[
    ("MS-7E83", 2), // MSI B850M POWER, confirmed on the bench
];

static BOARD_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)MS-[0-9A-Z]{4,}").unwrap());

/// How many DIMM sockets the board has, or 0 when unknown.
pub fn board_slot_count() -> i64 {
    let product = match wmi_util::query("Win32_BaseBoard") {
        Ok(boards) => boards.first().map(|board| board.str_or_empty("Product")).unwrap_or_default(),
        Err(_) => String::new(),
    };
    if let Some(found) = BOARD_CODE.find(&product) {
        let code = found.as_str().to_uppercase();
        if let Some((_, corrected)) = BOARD_MEMORY_SLOTS.iter().find(|(board, _)| *board == code)
            && *corrected != 0
        {
            return *corrected;
        }
    }
    if let Ok(arrays) = wmi_util::query("Win32_PhysicalMemoryArray")
        && let Some(array) = arrays.first()
    {
        return match array.get("MemoryDevices").filter(|value| truthy(value)) {
            None => 0,
            Some(value) => value.to_i64().unwrap_or(0),
        };
    }
    0
}

/// Populated sockets against the board's total, e.g. "2 of 2".
///
/// Two modules in a four-slot board behave differently from two in a
/// two-slot board, so the total is the point of the row -- which is why a
/// firmware count known to be wrong is corrected rather than printed. See
/// BOARD_MEMORY_SLOTS.
pub fn slots_used() -> Option<String> {
    let used = query_modules().len() as i64;
    let total = board_slot_count();
    if total == 0 || used > total {
        // A total that cannot hold the modules already counted is not a
        // total; say nothing rather than something impossible.
        return None;
    }
    Some(format!("{used} of {total}"))
}

/// The SPD identity read from this module: by serial, then part number.
fn identity_for(module: &DimmModule, identities: &[SpdIdentity]) -> Option<usize> {
    let serial = module.serial_number.trim().to_uppercase();
    if !serial.is_empty() {
        for (index, identity) in identities.iter().enumerate() {
            if identity.serial_number.trim().to_uppercase() == serial {
                return Some(index);
            }
        }
    }
    let part = module.part_number.trim().to_uppercase();
    identities.iter().position(|identity| identity.part_number.trim().to_uppercase() == part)
}

/// Put the DRAM maker and die each module's own SPD reports on its label.
///
/// `ic` starts as identify_dram_ic's answer, which is keyed on the part
/// number: right for a kit the table lists, "Unknown IC" for one it does not,
/// and wrong for one it lists wrongly -- the module is never asked. On DDR5
/// the SPD carries the DRAM maker and a stepping byte, so where it names the
/// maker that reading replaces the lookup. The die is the SPD's too when the
/// stepping decodes to one; a stepping no rule names keeps the table's die
/// only if the table agrees about the maker.
///
/// Returns copies, so the cached inventory keeps its own decode.
pub fn apply_spd_ic(modules: &[DimmModule], identities: &[SpdIdentity]) -> Vec<DimmModule> {
    let mut remaining: Vec<SpdIdentity> = identities.to_vec();
    let mut labelled = Vec::new();
    for module in modules {
        let mut module = module.clone();
        if let Some(index) = identity_for(&module, &remaining) {
            let identity = remaining.remove(index);
            let maker = identity.dram_manufacturer.trim().to_string();
            let named = !maker.is_empty() && maker != EM_DASH;
            if named && !maker.to_lowercase().starts_with("0x") {
                let mut die = identity.dram_die.as_deref().unwrap_or("").trim().to_string();
                if !die.ends_with("-die") {
                    let (table_maker, table_die) = split_ic(Some(&module.ic));
                    die = if table_maker == maker { table_die } else { EM_DASH.to_string() };
                }
                module.ic = if die != EM_DASH { format!("{maker} {die}") } else { format!("{maker} (die unknown)") };
            }
        }
        labelled.push(module);
    }
    labelled
}

/// [`read_modules`], with each module's IC label read from its SPD where the
/// SPD answers.
pub fn read_modules_with_spd_ic(refresh: bool) -> Vec<DimmModule> {
    let modules = read_modules(refresh);
    let identities = super::ddr5_spd::read_identity(None, false, None);
    apply_spd_ic(&modules, &identities)
}

/// Report one value for the whole set, or every distinct value.
///
/// Mixed kits are rare but real, and printing only the first module's rank or
/// die would quietly hide the mismatch that explains the timings.
pub fn shared_value<T>(modules: &[T], reader: impl Fn(&T) -> Option<String>) -> String {
    let mut values: Vec<String> = Vec::new();
    for module in modules {
        if let Some(value) = reader(module)
            && !value.is_empty()
            && value != EM_DASH
            && !values.contains(&value)
        {
            values.push(value);
        }
    }
    if values.is_empty() {
        return EM_DASH.to_string();
    }
    values.join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wmi_util::FakeWmi;

    fn fake_module(fields: &[(&str, WmiValue)]) -> WmiObject {
        let mut module = WmiObject::new()
            .with("Capacity", (16u64 * 1024 * 1024 * 1024).to_string())
            .with("PartNumber", "F5-6000J2636G16G")
            .with("Tag", "Physical Memory 2")
            .with("Manufacturer", "G.Skill")
            .with("SMBIOSMemoryType", 34i64)
            .with("Attributes", 1i64);
        for (name, value) in fields {
            module = module.with(name, value.clone());
        }
        module
    }

    fn read_with(modules: Vec<WmiObject>) -> Vec<DimmModule> {
        FakeWmi::new().class("Win32_PhysicalMemory", modules).install();
        query_modules()
    }

    // --- RankDecodeTest

    #[test]
    fn low_nibble_of_attributes_is_the_rank_count() {
        assert_eq!(rank_count(Some(&WmiValue::Int(1))), 1);
        assert_eq!(rank_count(Some(&WmiValue::Int(2))), 2);
        // Only the low nibble counts; upper bits carry other SMBIOS flags.
        assert_eq!(rank_count(Some(&WmiValue::Int(0x21))), 1);
    }

    #[test]
    fn missing_or_bad_attributes_read_as_no_rank_information() {
        assert_eq!(rank_count(None), 0);
        assert_eq!(rank_count(Some(&WmiValue::Null)), 0);
        assert_eq!(rank_count(Some(&WmiValue::Str(String::new()))), 0);
        assert_eq!(rank_count(Some(&WmiValue::Str("not a number".into()))), 0);
    }

    #[test]
    fn the_two_rank_spellings() {
        assert_eq!(rank_short(1), "SR");
        assert_eq!(rank_short(2), "DR");
        assert_eq!(rank_short(4), "4R");
        assert_eq!(rank_short(0), "N/A");
        assert_eq!(rank_numeric(1), "1R");
        assert_eq!(rank_numeric(2), "2R");
        assert_eq!(rank_numeric(0), EM_DASH);
    }

    // --- SplitIcTest

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    #[test]
    fn maker_and_die_split_off_the_combined_label() {
        assert_eq!(split_ic(Some("SK hynix A-die")), pair("SK hynix", "A-die"));
        assert_eq!(split_ic(Some("Samsung B-die")), pair("Samsung", "B-die"));
        assert_eq!(split_ic(Some("SK hynix CJR")), pair("SK hynix", "CJR"));
    }

    #[test]
    fn die_unknown_keeps_the_maker_and_drops_the_parenthetical() {
        assert_eq!(split_ic(Some("Micron (die unknown)")), pair("Micron", EM_DASH));
    }

    #[test]
    fn unidentified_ic_reports_neither() {
        assert_eq!(split_ic(Some("Unknown IC")), pair(EM_DASH, EM_DASH));
        assert_eq!(split_ic(Some("")), pair(EM_DASH, EM_DASH));
        assert_eq!(split_ic(None), pair(EM_DASH, EM_DASH));
    }

    // --- ReadModulesTest

    #[test]
    fn one_dict_per_installed_module() {
        let modules = read_with(vec![fake_module(&[]), fake_module(&[])]);
        assert_eq!(modules.len(), 2);
        let module = &modules[0];
        assert_eq!(module.capacity_gb, 16);
        assert_eq!(module.capacity, "16GB");
        assert_eq!(module.rank_count, 1);
        assert_eq!(module.rank, "1R");
        assert_eq!(module.ic, "SK hynix A-die");
        assert_eq!(module.part_number, "F5-6000J2636G16G");
    }

    #[test]
    fn missing_fields_do_not_raise() {
        let bare = fake_module(&[
            ("Capacity", WmiValue::Null),
            ("PartNumber", WmiValue::Null),
            ("Tag", WmiValue::Null),
            ("Manufacturer", WmiValue::Null),
            ("Attributes", WmiValue::Null),
        ]);
        let modules = read_with(vec![bare]);
        assert_eq!(modules[0].capacity_gb, 0);
        assert_eq!(modules[0].part_number, "Unknown");
        assert_eq!(modules[0].rank, EM_DASH);
    }

    #[test]
    fn a_failing_query_reports_no_modules() {
        FakeWmi::new().failing("Win32_PhysicalMemory").install();
        assert_eq!(query_modules(), vec![]);
    }

    #[test]
    fn an_explicit_connection_is_never_cached() {
        let first = read_with(vec![fake_module(&[])]);
        let second = read_with(vec![fake_module(&[]), fake_module(&[])]);
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 2);
    }

    #[test]
    fn the_shared_inventory_is_cached_until_refreshed() {
        FakeWmi::new().class("Win32_PhysicalMemory", vec![fake_module(&[])]).install();
        assert_eq!(read_modules(false).len(), 1);
        FakeWmi::new().class("Win32_PhysicalMemory", vec![fake_module(&[]), fake_module(&[])]).install();
        assert_eq!(read_modules(false).len(), 1);
        assert_eq!(read_modules(true).len(), 2);
    }

    // --- SharedValueTest

    #[test]
    fn a_matched_kit_reports_the_single_value() {
        let modules = read_with(vec![fake_module(&[]), fake_module(&[])]);
        assert_eq!(shared_value(&modules, |m| Some(m.rank.clone())), "1R");
    }

    #[test]
    fn a_mixed_kit_reports_every_distinct_value() {
        let modules = read_with(vec![fake_module(&[]), fake_module(&[("Attributes", WmiValue::Int(2))])]);
        assert_eq!(shared_value(&modules, |m| Some(m.rank.clone())), "1R / 2R");
    }

    #[test]
    fn nothing_known_reports_the_em_dash() {
        let none: &[DimmModule] = &[];
        assert_eq!(shared_value(none, |m| Some(m.rank.clone())), EM_DASH);
        let modules = read_with(vec![fake_module(&[])]);
        assert_eq!(shared_value(&modules, |_| Some(EM_DASH.to_string())), EM_DASH);
    }

    // --- SlotsUsedTest: populated sockets against the board's real total.

    fn fake_board(modules: Vec<WmiObject>, devices: i64, product: &str) {
        FakeWmi::new()
            .class("Win32_PhysicalMemory", modules)
            .class("Win32_PhysicalMemoryArray", vec![WmiObject::new().with("MemoryDevices", devices)])
            .class("Win32_BaseBoard", vec![WmiObject::new().with("Product", product)])
            .install();
    }

    #[test]
    fn the_firmware_count_is_used_when_the_board_is_not_listed() {
        fake_board(vec![fake_module(&[]), fake_module(&[])], 4, "MS-9999");
        assert_eq!(slots_used().as_deref(), Some("2 of 4"));
    }

    #[test]
    fn a_board_known_to_overstate_its_sockets_is_corrected() {
        // MS-7E83 declares four devices on a two-slot board.
        fake_board(vec![fake_module(&[]), fake_module(&[])], 4, "B850MPOWER (MS-7E83)");
        assert_eq!(slots_used().as_deref(), Some("2 of 2"));
        assert_eq!(board_slot_count(), 2);
    }

    #[test]
    fn the_board_code_is_matched_inside_the_product_string() {
        for product in ["B850MPOWER (MS-7E83)", "MS-7E83", "ms-7e83 rev 1.0"] {
            fake_board(vec![fake_module(&[])], 4, product);
            assert_eq!(slots_used().as_deref(), Some("1 of 2"), "{product}");
        }
    }

    #[test]
    fn no_reported_socket_count_reports_nothing() {
        // Better silent than implying the board has as many slots as modules.
        fake_board(vec![fake_module(&[])], 0, "MS-9999");
        assert_eq!(slots_used(), None);
    }

    #[test]
    fn a_total_that_cannot_hold_the_modules_is_rejected() {
        fake_board(vec![fake_module(&[]), fake_module(&[]), fake_module(&[])], 2, "MS-9999");
        assert_eq!(slots_used(), None);
    }

    #[test]
    fn a_failing_query_does_not_raise() {
        FakeWmi::new()
            .failing("Win32_PhysicalMemory")
            .failing("Win32_PhysicalMemoryArray")
            .failing("Win32_BaseBoard")
            .install();
        assert_eq!(slots_used(), None);
    }

    // --- SlotNameTest: slots come from the board's own DeviceLocator, never
    // from record order.

    fn slot(locator: &str) -> Option<String> {
        parse_slot(locator, "")
    }

    #[test]
    fn the_common_desktop_form() {
        assert_eq!(slot("Controller0-DIMMA2").as_deref(), Some("A2"));
        assert_eq!(slot("Controller1-DIMMB2").as_deref(), Some("B2"));
    }

    #[test]
    fn a_bare_locator() {
        assert_eq!(slot("DIMM_A1").as_deref(), Some("A1"));
        assert_eq!(slot("DIMMB2").as_deref(), Some("B2"));
    }

    #[test]
    fn the_channel_and_index_form_numbers_from_one() {
        assert_eq!(slot("Controller0-ChannelA-DIMM0").as_deref(), Some("A1"));
        assert_eq!(slot("Controller1-ChannelB-DIMM1").as_deref(), Some("B2"));
    }

    #[test]
    fn the_controller_and_index_form_numbers_from_one() {
        // ASUS ROG MAXIMUS Z790 APEX writes this: no channel letter anywhere,
        // so the controller index supplies it. Left unparsed the bottom DIMM
        // strip drew nothing and the channel columns stayed ChA/ChB.
        assert_eq!(slot("Controller0-DIMM0").as_deref(), Some("A1"));
        assert_eq!(slot("Controller1-DIMM0").as_deref(), Some("B1"));
        assert_eq!(slot("Controller0-DIMM1").as_deref(), Some("A2"));
    }

    #[test]
    fn a_named_slot_wins_over_the_controller_index() {
        // Both shapes carry "Controller<n>"; the board's own letter is the
        // better answer wherever firmware bothered to write one.
        assert_eq!(slot("Controller1-DIMMA2").as_deref(), Some("A2"));
        assert_eq!(slot("Controller0-ChannelB-DIMM1").as_deref(), Some("B2"));
    }

    #[test]
    fn a_lowercase_locator_still_reads() {
        assert_eq!(slot("controller0-dimma2").as_deref(), Some("A2"));
        assert_eq!(slot("controller1-dimm0").as_deref(), Some("B1"));
    }

    #[test]
    fn an_unreadable_locator_is_not_guessed() {
        for value in ["", "Unknown", "SODIMM"] {
            assert_eq!(slot(value), None, "{value}");
        }
    }

    #[test]
    fn channel_of_reads_the_leading_letter() {
        assert_eq!(channel_of(Some("A2")).as_deref(), Some("A"));
        assert_eq!(channel_of(Some("B1")).as_deref(), Some("B"));
        assert_eq!(channel_of(None), None);
    }

    #[test]
    fn slots_group_and_sort_by_channel() {
        let modules = [Some("B2".to_string()), Some("A2".into()), Some("A1".into()), None];
        assert_eq!(
            slots_by_channel(&modules),
            BTreeMap::from([("A".to_string(), vec!["A1".to_string(), "A2".into()]), ("B".into(), vec!["B2".into()])])
        );
    }

    #[test]
    fn the_records_that_used_to_be_mislabelled() {
        // The MSI Z790-P returns its two modules as records 1 and 3. A table
        // keyed on that position called them B1 and A1: wrong numbers, and the
        // channels swapped. The locator gets both right.
        let modules = [slot("Controller0-DIMMA2"), slot("Controller1-DIMMB2")];
        assert_eq!(
            slots_by_channel(&modules),
            BTreeMap::from([("A".to_string(), vec!["A2".to_string()]), ("B".into(), vec!["B2".into()])])
        );
    }

    #[test]
    fn the_gigabyte_bank_label_names_the_channel() {
        assert_eq!(parse_slot("DIMM 1", "P0 CHANNEL A").as_deref(), Some("A1"));
        assert_eq!(parse_slot("DIMM 2", "P0 CHANNEL B").as_deref(), Some("B2"));
    }

    // --- SlotDecodeTest

    #[test]
    fn decode_carries_the_slot_and_channel() {
        let modules = read_with(vec![fake_module(&[("DeviceLocator", "Controller1-DIMMB2".into())])]);
        assert_eq!(modules[0].slot.as_deref(), Some("B2"));
        assert_eq!(modules[0].channel.as_deref(), Some("B"));
        assert_eq!(modules[0].device_locator, "Controller1-DIMMB2");
    }

    #[test]
    fn a_module_without_a_locator_reports_no_slot() {
        let modules = read_with(vec![fake_module(&[("DeviceLocator", "".into())])]);
        assert_eq!(modules[0].slot, None);
        assert_eq!(modules[0].channel, None);
    }

    #[test]
    fn duplicate_channel_labels_are_disambiguated_by_controller() {
        let modules = read_with(vec![
            fake_module(&[("DeviceLocator", "Controller0-ChannelA-DIMM1".into())]),
            fake_module(&[("DeviceLocator", "Controller1-ChannelA-DIMM1".into())]),
        ]);
        assert_eq!(
            modules.iter().map(|m| (m.slot.clone().unwrap(), m.channel.clone().unwrap())).collect::<Vec<_>>(),
            vec![("A2".to_string(), "A".to_string()), ("B2".into(), "B".into())]
        );
    }

    #[test]
    fn unique_channel_label_remains_authoritative() {
        let modules = read_with(vec![fake_module(&[("DeviceLocator", "Controller0-ChannelB-DIMM1".into())])]);
        assert_eq!(modules[0].slot.as_deref(), Some("B2"));
    }

    // --- SerialNumberTest: SMBIOS carries the serial firmware read off the
    // module at POST.
    //
    // It is the only route to it on DDR4, where the SPD path that reads the
    // serial directly is a DDR5 hub protocol and refuses to run.

    fn serial(value: WmiValue) -> String {
        read_with(vec![fake_module(&[("SerialNumber", value)])])[0].serial_number.clone()
    }

    #[test]
    fn a_real_serial_is_carried_through() {
        assert_eq!(serial("0000ABCD".into()), "0000ABCD");
    }

    #[test]
    fn an_all_zero_placeholder_is_not_a_serial() {
        // What the Z790-P bench reports for both sticks. Printing it would put
        // a serial of 00000000 on a module that never gave one.
        assert_eq!(serial("00000000".into()), "");
    }

    #[test]
    fn an_all_f_placeholder_is_not_a_serial_either() {
        assert_eq!(serial("FFFFFFFF".into()), "");
    }

    #[test]
    fn a_serial_that_merely_ends_in_zero_survives() {
        // The placeholder test strips zeroes to decide; it must not strip
        // them from the answer.
        assert_eq!(serial("12345670".into()), "12345670");
    }

    #[test]
    fn a_missing_serial_is_empty_rather_than_an_error() {
        assert_eq!(serial(WmiValue::Null), "");
    }

    // --- SpdIcTest: the module label names the DRAM its own SPD reports,
    // not a table's.

    fn module(ic: &str, serial: &str) -> DimmModule {
        DimmModule {
            part_number: "TMXFL1680838KWK".into(),
            serial_number: serial.into(),
            ic: ic.into(),
            ..DimmModule::default()
        }
    }

    fn identity(maker: &str, die: &str, serial: &str) -> SpdIdentity {
        SpdIdentity {
            part_number: "TMXFL1680838KWK".into(),
            serial_number: serial.into(),
            dram_manufacturer: maker.into(),
            dram_die: Some(die.into()),
            ..SpdIdentity::default()
        }
    }

    #[test]
    fn the_spd_reading_replaces_the_part_number_lookup() {
        // A kit the table does not list, or lists wrongly, still reads right.
        let labelled = apply_spd_ic(&[module("Unknown IC", "0B354996")], &[identity("SK hynix", "M-die", "0B354996")]);
        assert_eq!(labelled[0].ic, "SK hynix M-die");
        assert_eq!(split_ic(Some(&labelled[0].ic)), pair("SK hynix", "M-die"));
    }

    #[test]
    fn each_module_is_matched_to_its_own_spd_by_serial() {
        let modules = [module("Unknown IC", "0B354996"), module("Unknown IC", "0B354997")];
        let identities = [identity("SK hynix", "M-die", "0B354997"), identity("SK hynix", "A-die", "0B354996")];
        let labelled = apply_spd_ic(&modules, &identities);
        assert_eq!(
            labelled.iter().map(|m| m.ic.as_str()).collect::<Vec<_>>(),
            vec!["SK hynix A-die", "SK hynix M-die"]
        );
    }

    #[test]
    fn an_undecoded_stepping_keeps_only_an_agreeing_tables_die() {
        let kept = apply_spd_ic(&[module("Samsung B-die", "0B354996")], &[identity("Samsung", "0x00", "0B354996")]);
        assert_eq!(kept[0].ic, "Samsung B-die");
        let dropped = apply_spd_ic(&[module("Samsung B-die", "0B354996")], &[identity("Micron", "0x00", "0B354996")]);
        assert_eq!(split_ic(Some(&dropped[0].ic)), pair("Micron", EM_DASH));
    }

    #[test]
    fn no_spd_answer_leaves_the_lookup_in_place() {
        let unread = apply_spd_ic(&[module("SK hynix A-die", "0B354996")], &[]);
        assert_eq!(unread[0].ic, "SK hynix A-die");
        let raw = apply_spd_ic(&[module("SK hynix A-die", "0B354996")], &[identity("0x80AD", "A-die", "0B354996")]);
        assert_eq!(raw[0].ic, "SK hynix A-die");
    }

    #[test]
    fn the_cached_inventory_is_not_rewritten() {
        let original = module("Unknown IC", "0B354996");
        apply_spd_ic(std::slice::from_ref(&original), &[identity("SK hynix", "A-die", "0B354996")]);
        assert_eq!(original.ic, "Unknown IC");
    }
}
