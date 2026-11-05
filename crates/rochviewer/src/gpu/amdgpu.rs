//! A Radeon through the kernel's `amdgpu` driver.
//!
//! On Windows this comes from AMD's driver libraries, ADL and ADLX, and the
//! display class registry key. Linux has neither, and needs neither: the driver
//! publishes all of it in sysfs, and the rest through its own query ioctl, for
//! any user. So this answers in the same shapes the ADL and ADLX modules answer
//! in, and the rows above them do not change.
//!
//! The one rule that is different: **a card that is asleep is left asleep.** The
//! kernel powers an idle card with no display attached down to D3cold, and reading
//! its sensors would wake it. On some machines a card fails to come back, and
//! everything that touches it then blocks in the kernel, unkillably. So a card
//! not reporting itself active simply has no readings.

use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use crate::amd::adl::{AdlMemory, format_sensor};
use crate::system_identity::PciLocation;

const PCI_ROOT: &str = "/sys/bus/pci/devices";
const AMD_VENDOR: &str = "0x1002";

/// `DRM_IOCTL_AMDGPU_INFO`, from `drm/amdgpu_drm.h`, and its query for
/// `struct drm_amdgpu_info_device`. These are the kernel's stable ABI, and the
/// offsets below were confirmed against that header with a compiled probe on the
/// Z890 bench: device 0x744C, revision 0xCC, 84 compute units, type 9, 320 bits.
const DRM_IOCTL_AMDGPU_INFO: libc::c_ulong = 0x4020_6445;
const AMDGPU_INFO_DEV_INFO: u32 = 0x16;
const DEVICE_INFO_SIZE: usize = 440;
const OFFSET_CU_ACTIVE_NUMBER: usize = 48;
const OFFSET_VRAM_TYPE: usize = 176;
const OFFSET_VRAM_BIT_WIDTH: usize = 180;

/// Every compute unit since GCN has had 64 stream processors: four 16-wide SIMDs
/// then, two 32-wide ones now. It is why "84 compute units" is what AMD sells as
/// the 7900 XT's 5376.
const SHADERS_PER_COMPUTE_UNIT: u32 = 64;

#[repr(C)]
struct DrmAmdgpuInfo {
    return_pointer: u64,
    return_size: u32,
    query: u32,
    /// The per-query union, unused by this query.
    arguments: [u32; 4],
}

const _: () = assert!(std::mem::size_of::<DrmAmdgpuInfo>() == 32);

/// What the driver reports about the card's silicon and memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub compute_units: u32,
    pub vram_type: u32,
    pub vram_bit_width: u32,
}

impl DeviceInfo {
    pub fn shaders(&self) -> Option<u32> {
        self.compute_units.checked_mul(SHADERS_PER_COMPUTE_UNIT).filter(|count| *count != 0)
    }

    pub fn bus_width(&self) -> Option<String> {
        (self.vram_bit_width != 0).then(|| format!("{} bit", self.vram_bit_width))
    }
}

/// `AMDGPU_VRAM_TYPE_*` by name. An unlisted code is no reading, not a guess.
pub fn vram_type_name(code: u32) -> Option<&'static str> {
    match code {
        1 => Some("GDDR"),
        2 => Some("DDR2"),
        3 => Some("GDDR3"),
        4 => Some("GDDR4"),
        5 => Some("GDDR5"),
        6 => Some("HBM"),
        7 => Some("DDR3"),
        8 => Some("DDR4"),
        9 => Some("GDDR6"),
        10 => Some("DDR5"),
        11 => Some("LPDDR4"),
        12 => Some("LPDDR5"),
        13 => Some("HBM3E"),
        14 => Some("HBM4"),
        _ => None,
    }
}

/// The driver's lowercase vendor word, as the rest of this project writes it.
pub fn vram_vendor_name(word: &str) -> Option<String> {
    let word = word.trim();
    if word.is_empty() || word.eq_ignore_ascii_case("unknown") {
        return None;
    }
    Some(match word.to_ascii_lowercase().as_str() {
        "samsung" => "Samsung".into(),
        "hynix" => "SK hynix".into(),
        "micron" => "Micron".into(),
        "elpida" => "Elpida".into(),
        "etron" => "Etron".into(),
        "nanya" => "Nanya".into(),
        "qimonda" => "Qimonda".into(),
        "infineon" => "Infineon".into(),
        "winbond" => "Winbond".into(),
        "esmt" => "ESMT".into(),
        other => other.to_string(),
    })
}

fn read(path: impl AsRef<Path>) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn read_number<T: std::str::FromStr>(path: impl AsRef<Path>) -> Option<T> {
    read(path)?.parse().ok()
}

fn location_path(location: PciLocation) -> PathBuf {
    let (bus, device, function) = location;
    PathBuf::from(format!("{PCI_ROOT}/0000:{bus:02x}:{device:02x}.{function}"))
}

/// The AMD display functions, the one driving the screen first.
fn amd_display_functions() -> Vec<PathBuf> {
    let Ok(devices) = std::fs::read_dir(PCI_ROOT) else {
        return Vec::new();
    };
    let mut found: Vec<(bool, PathBuf)> = devices
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            read(path.join("vendor")).as_deref() == Some(AMD_VENDOR)
                && read(path.join("class")).is_some_and(|class| class.starts_with("0x03"))
        })
        .map(|path| (read(path.join("boot_vga")).as_deref() == Some("1"), path))
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().map(|(_, path)| path).collect()
}

/// The card at `location`, or the primary one without a location.
pub fn device_path(location: Option<PciLocation>) -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    match location {
        Some(location) => Some(location_path(location)).filter(|path| path.exists()),
        None => amd_display_functions().into_iter().next(),
    }
}

/// The card with this device id and revision, the primary one first.
pub fn device_path_for(device: u16, revision: u8) -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let wanted_device = format!("0x{device:04x}");
    let wanted_revision = format!("0x{revision:02x}");
    amd_display_functions().into_iter().find(|path| {
        read(path.join("device")).as_deref() == Some(&wanted_device)
            && read(path.join("revision")).as_deref() == Some(&wanted_revision)
    })
}

/// Whether a card can be read without waking it. A device that does no runtime
/// power management has nothing to wake, and says nothing.
fn is_awake(device: &Path) -> bool {
    read(device.join("power/runtime_status")).is_none_or(|status| status == "active")
}

fn hwmon_directory(device: &Path) -> Option<PathBuf> {
    std::fs::read_dir(device.join("hwmon")).ok()?.flatten().map(|entry| entry.path()).next()
}

/// The hwmon channel of `prefix` whose label is `label`: `temp2` for "junction".
fn labelled(hwmon: &Path, prefix: &str, label: &str) -> Option<String> {
    (1..=8).map(|index| format!("{prefix}{index}")).find(|channel| {
        read(hwmon.join(format!("{channel}_label"))).as_deref() == Some(label)
    })
}

/// `{label: display text}` in the labels [`crate::amd::adl::PMLOG_SENSORS`] uses,
/// for one card, or empty when it is asleep or is not an amdgpu device.
pub fn read_sensors(location: Option<PciLocation>) -> BTreeMap<&'static str, String> {
    let mut found = BTreeMap::new();
    let Some(device) = device_path(location) else { return found };
    if !is_awake(&device) {
        return found;
    }
    let Some(hwmon) = hwmon_directory(&device) else { return found };

    let mut clock = |row: &'static str, label: &str| {
        if let Some(channel) = labelled(&hwmon, "freq", label)
            && let Some(hertz) = read_number::<i64>(hwmon.join(format!("{channel}_input")))
        {
            found.insert(row, format_sensor((hertz / 1_000_000) as i32, "MHz", 1.0));
        }
    };
    clock("GPU Clock", "sclk");
    clock("GPU Memory Clock", "mclk");

    let mut temperature = |row: &'static str, label: &str| {
        if let Some(channel) = labelled(&hwmon, "temp", label)
            && let Some(millidegrees) = read_number::<i64>(hwmon.join(format!("{channel}_input")))
        {
            found.insert(row, format_sensor((millidegrees / 1000) as i32, "\u{00b0}C", 1.0));
        }
    };
    temperature("GPU Temperature", "edge");
    temperature("GPU Hot Spot", "junction");
    temperature("GPU Memory Junction", "mem");

    if let Some(rpm) = read_number::<i32>(hwmon.join("fan1_input")) {
        found.insert("GPU Fan", format_sensor(rpm, "RPM", 1.0));
    }
    if let Some(duty) = read_number::<i64>(hwmon.join("pwm1")) {
        let full = read_number::<i64>(hwmon.join("pwm1_max")).filter(|max| *max > 0).unwrap_or(255);
        found.insert("GPU Fan PWM", format_sensor((duty * 100 / full) as i32, "%", 1.0));
    }
    if let Some(busy) = read_number::<i32>(device.join("gpu_busy_percent")) {
        found.insert("GPU Utilization", format_sensor(busy, "%", 1.0));
    }
    if let Some(millivolts) = read_number::<i32>(hwmon.join("in0_input")) {
        found.insert("GPU Core Voltage", format_sensor(millivolts, "V", 0.001));
    }
    // Averaged on most cards, instantaneous on some newer ones, in microwatts.
    let power = read_number::<i64>(hwmon.join("power1_average"))
        .or_else(|| read_number::<i64>(hwmon.join("power1_input")));
    if let Some(microwatts) = power {
        found.insert("GPU Board Power (TBP)", format_sensor((microwatts / 1_000_000) as i32, "W", 1.0));
    }
    found
}

/// [`read_sensors`] for the primary card, with the two rows that are not sensors:
/// the board's power limit, and how much of its memory is in use.
pub fn read_board_sensors() -> BTreeMap<&'static str, String> {
    let mut found = read_sensors(None);
    if found.is_empty() {
        return found;
    }
    if let Some(watts) = board_power_limit() {
        found.insert("GPU Board Power Limit", format!("{watts} W"));
    }
    if let Some(bytes) = device_path(None).and_then(|device| read_number::<u64>(device.join("mem_info_vram_used"))) {
        found.insert("GPU Memory Used", format!("{} MB", bytes / (1024 * 1024)));
    }
    found
}

/// The driver's answer about the card's memory, or empty.
pub fn read_memory(location: Option<PciLocation>) -> AdlMemory {
    let mut found = AdlMemory::default();
    let Some(device) = device_path(location) else { return found };
    if !is_awake(&device) {
        return found;
    }
    found.memory_vendor = read(device.join("mem_info_vram_vendor")).and_then(|word| vram_vendor_name(&word));
    found.memory_bytes = read_number::<i64>(device.join("mem_info_vram_total")).filter(|bytes| *bytes > 0);
    found.memory_type = device_info(&device).and_then(|info| vram_type_name(info.vram_type)).map(str::to_string);
    found
}

/// The card's board power ceiling in watts, or `None`.
pub fn board_power_limit() -> Option<i32> {
    let device = device_path(None)?;
    if !is_awake(&device) {
        return None;
    }
    let microwatts: i64 = read_number(hwmon_directory(&device)?.join("power1_cap"))?;
    (microwatts > 0).then_some((microwatts / 1_000_000) as i32)
}

/// What the driver's query ioctl says, through the card's render node, which an
/// ordinary user can open.
pub fn device_info(device: &Path) -> Option<DeviceInfo> {
    let node = std::fs::read_dir(device.join("drm"))
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .find(|name| name.starts_with("renderD"))?;
    let file = std::fs::OpenOptions::new().read(true).write(true).open(format!("/dev/dri/{node}")).ok()?;
    let mut buffer = [0u8; DEVICE_INFO_SIZE];
    let mut request = DrmAmdgpuInfo {
        return_pointer: buffer.as_mut_ptr() as u64,
        return_size: DEVICE_INFO_SIZE as u32,
        query: AMDGPU_INFO_DEV_INFO,
        arguments: [0; 4],
    };
    // SAFETY: the request has the layout the ioctl documents, and it points at a
    // live buffer of the size it states, which the kernel fills at most that far.
    if unsafe { libc::ioctl(file.as_raw_fd(), DRM_IOCTL_AMDGPU_INFO, &mut request as *mut DrmAmdgpuInfo) } != 0 {
        return None;
    }
    Some(parse_device_info(&buffer))
}

fn parse_device_info(buffer: &[u8; DEVICE_INFO_SIZE]) -> DeviceInfo {
    let dword = |offset: usize| u32::from_ne_bytes(buffer[offset..offset + 4].try_into().expect("four bytes"));
    DeviceInfo {
        compute_units: dword(OFFSET_CU_ACTIVE_NUMBER),
        vram_type: dword(OFFSET_VRAM_TYPE),
        vram_bit_width: dword(OFFSET_VRAM_BIT_WIDTH),
    }
}

/// `DeviceInfo` for the card at `location`, or the primary one.
pub fn read_device_info(location: Option<PciLocation>) -> Option<DeviceInfo> {
    let device = device_path(location)?;
    if !is_awake(&device) {
        return None;
    }
    device_info(&device)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_driver_codes_name_what_the_header_says() {
        assert_eq!(vram_type_name(9), Some("GDDR6"));
        assert_eq!(vram_type_name(5), Some("GDDR5"));
        assert_eq!(vram_type_name(0), None, "unknown is no reading");
        assert_eq!(vram_type_name(99), None);
    }

    #[test]
    fn memory_vendors_read_as_the_rest_of_the_project_writes_them() {
        assert_eq!(vram_vendor_name("samsung\n").as_deref(), Some("Samsung"));
        assert_eq!(vram_vendor_name("hynix").as_deref(), Some("SK hynix"));
        assert_eq!(vram_vendor_name("unknown"), None);
        assert_eq!(vram_vendor_name(""), None);
        assert_eq!(vram_vendor_name("someoneelse").as_deref(), Some("someoneelse"), "as reported, not hidden");
    }

    #[test]
    fn the_device_info_block_decodes_at_the_headers_offsets() {
        // What the Z890 bench's RX 7900 XT answers.
        let mut buffer = [0u8; DEVICE_INFO_SIZE];
        buffer[OFFSET_CU_ACTIVE_NUMBER..][..4].copy_from_slice(&84u32.to_ne_bytes());
        buffer[OFFSET_VRAM_TYPE..][..4].copy_from_slice(&9u32.to_ne_bytes());
        buffer[OFFSET_VRAM_BIT_WIDTH..][..4].copy_from_slice(&320u32.to_ne_bytes());
        let info = parse_device_info(&buffer);
        assert_eq!(info, DeviceInfo { compute_units: 84, vram_type: 9, vram_bit_width: 320 });
        assert_eq!(info.shaders(), Some(5376), "AMD's published figure for the part");
        assert_eq!(info.bus_width().as_deref(), Some("320 bit"));
    }

    #[test]
    fn an_empty_answer_is_no_reading() {
        let info = parse_device_info(&[0u8; DEVICE_INFO_SIZE]);
        assert_eq!(info.shaders(), None);
        assert_eq!(info.bus_width(), None);
    }

    #[test]
    fn a_location_becomes_its_sysfs_path() {
        assert_eq!(location_path((0x04, 0, 0)), Path::new("/sys/bus/pci/devices/0000:04:00.0"));
    }

    #[test]
    fn the_machine_is_not_read_under_test() {
        assert_eq!(device_path(None), None);
        assert_eq!(read_memory(None), AdlMemory::default());
        assert!(read_sensors(None).is_empty());
    }
}
