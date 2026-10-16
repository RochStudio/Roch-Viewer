//! What AMD's own driver library will tell us about the card.
//!
//! The mirror of what the Intel profile does with NVAPI: the vendor ships a
//! user-mode library alongside its driver, and it answers questions no register
//! this project can reach will. atiadlxx.dll is that library for Radeon.
//!
//! It is the only source found for the card's memory vendor. Everything else was
//! tried first and closed: the expansion ROM BAR is unassigned, so there is no
//! VBIOS image to parse; MC_SEQ_MISC0, where AMD tools used to read the vendor
//! nibble, answers all-ones on RDNA4 -- and that is a real answer, since other
//! offsets in the same aperture return live values; and the driver's registry
//! subtree carries no vendor string anywhere under it.
//!
//! Loaded lazily and failing quietly. A machine with no Radeon driver has no
//! atiadlxx.dll, and that is not an error -- it is a machine this module has
//! nothing to say about.

use std::collections::BTreeMap;
use std::ffi::{c_char, c_void};

use crate::system_identity::{PciLocation, py_strip};

pub const ADL_MAX_PATH: usize = 256;
pub const ADL_OK: i32 = 0;

/// ADL's VRAM vendor codes. Only the one this bench answers is named, on the
/// same rule the NVAPI tables follow: an unmeasured code prints as itself
/// rather than being filled in from a list that has never been checked here.
///
/// Code 1 is confirmed rather than assumed. ADL reports it for this card and
/// GPU-Z, reading the same VBIOS by its own route, names the card's memory
/// Samsung. Two independent tools agreeing on one card is what this entry
/// rests on -- which is why the rest of the list is still absent.
pub const VRAM_VENDORS: &[(i32, &str)] = &[(1, "Samsung")];

pub const MAX_PMLOG_SENSORS: usize = 256;

/// ADL's PMLog sensor indices. Four of these were matched exactly against
/// HWiNFO reading the same card at the same moment -- fan 1825 RPM, fan PWM
/// 50%, GPU utilisation 4% and memory clock 909 MHz -- which is what says the
/// enumeration is aligned rather than merely plausible. The rest are the
/// neighbouring entries of the same enum, and their values agreed to within
/// the degree ADL rounds to.
///
/// Twenty sensors is all this card exposes through ADL, and that was tested
/// rather than assumed. ADL2_Adapter_PMLog_Support_Get names exactly these
/// twenty, and starting a full PMLog logging session returned the same twenty,
/// so the session is not used: it is a stateful call that buys nothing over
/// the plain query.
///
/// Eleven of those twenty were left unread for a while, and board power was
/// declared unreachable without ever being printed. It was index 73 the whole
/// time. A GPU load settled it: 37 W at rest, then pinned at 361-371 W for two
/// minutes at full utilisation -- which is HWiNFO's 363 W TBP limit, because a
/// saturated card sits on its limit. A limit reading would not have moved.
///
/// The same load confirmed the two rows that were flagged here as the ones to
/// doubt. Edge and hotspot both read 31 at rest, so nothing separated them.
/// Under load edge went to 56 and hotspot to 92 -- a rise of 25 against 61.
/// The hotspot outruns the edge by better than two to one, which is the shape
/// that says these labels are the right way round.
///
/// Still not reachable, and checked rather than assumed: the TBP limit itself
/// and the GPU fabric clock. Of the remaining unmapped indices, 38, 39 and
/// 46-49 read zero throughout, and 40, 41 and 58 never moved off 4, 16 and 5
/// across the whole idle-load-idle round trip. None of them is the 363 W limit
/// or the 2401 MHz fabric clock HWiNFO shows. Those two come from ADLX, the
/// newer library, present here as amdadlx64.dll and a different interface
/// again. Overdrive6_CurrentPower_Get answers 0 W: this card is Overdrive 8.
///
/// (index, label, unit, scale). Scale turns ADL's integer into the unit.
pub const PMLOG_SENSORS: &[(usize, &str, &str, f64)] = &[
    (1, "GPU Clock", "MHz", 1.0),
    (2, "GPU Memory Clock", "MHz", 1.0),
    (8, "GPU Temperature", "°C", 1.0),
    (9, "GPU Memory Junction", "°C", 1.0),
    (14, "GPU Fan", "RPM", 1.0),
    (15, "GPU Fan PWM", "%", 1.0),
    (19, "GPU Utilization", "%", 1.0),
    (21, "GPU Core Voltage", "V", 0.001),
    (27, "GPU Hot Spot", "°C", 1.0),
    (73, "GPU Board Power (TBP)", "W", 1.0),
];

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SensorData {
    supported: i32,
    value: i32,
}

/// ADLPMLogDataOutput.
#[repr(C)]
#[derive(Clone, Copy)]
struct PMLogDataOutput {
    size: i32,
    sensors: [SensorData; MAX_PMLOG_SENSORS],
}

const _: () = assert!(std::mem::size_of::<PMLogDataOutput>() == 2052);

/// AdapterInfo.
#[repr(C)]
#[derive(Clone, Copy)]
struct AdapterInfo {
    i_size: i32,
    i_adapter_index: i32,
    str_udid: [c_char; ADL_MAX_PATH],
    i_bus_number: i32,
    i_device_number: i32,
    i_function_number: i32,
    i_vendor_id: i32,
    str_adapter_name: [c_char; ADL_MAX_PATH],
    str_display_name: [c_char; ADL_MAX_PATH],
    i_present: i32,
    i_exist: i32,
    str_driver_path: [c_char; ADL_MAX_PATH],
    str_driver_path_ext: [c_char; ADL_MAX_PATH],
    str_pnp_string: [c_char; ADL_MAX_PATH],
    i_os_display_index: i32,
}

const _: () = assert!(std::mem::size_of::<AdapterInfo>() == 1572);

/// ADLMemoryInfoX4. iVramVendorRevId is the field that matters here.
#[repr(C)]
#[derive(Clone, Copy)]
struct MemoryInfoX4 {
    i_memory_size: i64,
    str_memory_type: [c_char; ADL_MAX_PATH],
    i_memory_bandwidth: i64,
    i_hyper_memory_size: i64,
    i_invisible_memory_size: i64,
    i_visible_memory_size: i64,
    i_vram_vendor_rev_id: i32,
    i_memory_bandwidth_x2: i32,
    i_memory_type: i32,
}

const _: () = assert!(std::mem::size_of::<MemoryInfoX4>() == 312);

/// A zero-filled value of a plain-data ADL structure, as ctypes made them.
fn zeroed<T: Copy>() -> T {
    // SAFETY: only used for the #[repr(C)] structures above, which hold
    // integers and byte arrays, for which all-zero bytes are a valid value.
    unsafe { std::mem::zeroed() }
}

type Context = *mut c_void;

// ADL is called as CDLL, the cdecl convention; on x64 there is only one.
type MallocCallback = unsafe extern "C" fn(i32) -> *mut c_void;
type MainControlCreateFn = unsafe extern "C" fn(MallocCallback, i32, *mut Context) -> i32;
type MainControlDestroyFn = unsafe extern "C" fn(Context) -> i32;
type NumberOfAdaptersFn = unsafe extern "C" fn(Context, *mut i32) -> i32;
type AdapterInfoFn = unsafe extern "C" fn(Context, *mut AdapterInfo, i32) -> i32;
type MemoryInfoX4Fn = unsafe extern "C" fn(Context, i32, *mut MemoryInfoX4) -> i32;
type QueryPMLogDataFn = unsafe extern "C" fn(Context, i32, *mut PMLogDataOutput) -> i32;

/// The exports of atiadlxx.dll this module calls; `None` for one the
/// library does not have.
#[derive(Clone, Copy, Default)]
struct AdlApi {
    main_control_create: Option<MainControlCreateFn>,
    main_control_destroy: Option<MainControlDestroyFn>,
    number_of_adapters_get: Option<NumberOfAdaptersFn>,
    adapter_info_get: Option<AdapterInfoFn>,
    memory_info_x4_get: Option<MemoryInfoX4Fn>,
    new_query_pmlog_data_get: Option<QueryPMLogDataFn>,
}

#[cfg(test)]
thread_local! {
    /// The exports a test stands in for atiadlxx.dll.
    static TEST_ADL: std::cell::Cell<Option<AdlApi>> = const { std::cell::Cell::new(None) };
}

fn load_adl() -> Option<AdlApi> {
    #[cfg(test)]
    {
        // Unit tests never load a vendor library.
        TEST_ADL.with(|slot| slot.get())
    }
    #[cfg(not(test))]
    {
        use libloading::os::windows::{LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, Library};

        static LOADED: std::sync::Mutex<Option<AdlApi>> = std::sync::Mutex::new(None);
        let mut loaded = LOADED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(api) = *loaded {
            return Some(api);
        }

        // SAFETY: loading the driver's library runs its initialisation, as
        // loading it through ctypes did; the default search directories are
        // the ones ctypes uses for a bare name.
        let library = unsafe { Library::load_with_flags("atiadlxx.dll", LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) }.ok()?;
        fn export<T: Copy>(library: &Library, name: &str) -> Option<T> {
            // SAFETY: each export is named with its ADL SDK signature.
            unsafe { library.get::<T>(name) }.ok().map(|found| *found)
        }
        let api = AdlApi {
            main_control_create: export(&library, "ADL2_Main_Control_Create"),
            main_control_destroy: export(&library, "ADL2_Main_Control_Destroy"),
            number_of_adapters_get: export(&library, "ADL2_Adapter_NumberOfAdapters_Get"),
            adapter_info_get: export(&library, "ADL2_Adapter_AdapterInfo_Get"),
            memory_info_x4_get: export(&library, "ADL2_Adapter_MemoryInfoX4_Get"),
            new_query_pmlog_data_get: export(&library, "ADL2_New_QueryPMLogData_Get"),
        };
        // Kept loaded for the life of the process, as ctypes keeps every
        // library it loads; unloading the driver's library once a tick is
        // not something the Python ever did.
        std::mem::forget(library);
        *loaded = Some(api);
        Some(api)
    }
}

/// ADL allocates through the caller, so it gets a buffer it can keep.
///
/// Zero-filled, as ctypes' create_string_buffer was, and deliberately never
/// freed: ADL may hold on to it for the life of the context. (The Python's
/// buffer was released as soon as the callback returned, which left ADL a
/// dangling pointer; keeping it is what the callback was meant to do.)
unsafe extern "C" fn allocate(size: i32) -> *mut c_void {
    let Ok(size) = usize::try_from(size) else {
        return std::ptr::null_mut();
    };
    let Ok(layout) = std::alloc::Layout::from_size_align(size.max(1), 16) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the layout has a non-zero size.
    unsafe { std::alloc::alloc_zeroed(layout).cast() }
}

/// Name a VRAM vendor code, or print the code when it is not named.
pub fn vram_vendor(code: Option<i32>) -> Option<String> {
    let code = code?;
    Some(
        VRAM_VENDORS
            .iter()
            .find(|(known, _)| *known == code)
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| format!("Vendor {code}")),
    )
}

/// Open ADL, find the card, hand it to `work`, and always close.
///
/// Keyed on the PCI location when one is given, so the right adapter is
/// asked on a machine with more than one. ADL lists an entry per display
/// output, so several share a location and the first is enough.
///
/// Shared because both readers need the same six calls of boilerplate, and
/// the one that matters is the last: the context is destroyed whatever
/// happens after it was created, or a failed read leaks a driver handle every
/// time it is retried.
fn with_adapter<T>(
    location: Option<PciLocation>,
    work: impl FnOnce(&AdlApi, Context, i32) -> Option<T>,
) -> Option<T> {
    let api = load_adl()?;

    let create = api.main_control_create?;
    let mut context: Context = std::ptr::null_mut();
    // SAFETY: ADL2_Main_Control_Create takes the allocation callback, the
    // enumerate-connected-adapters flag and an out context handle.
    if unsafe { create(allocate, 1, &mut context) } != ADL_OK {
        return None;
    }

    let result = find_adapter(&api, context, location).and_then(|index| work(&api, context, index));

    if let Some(destroy) = api.main_control_destroy {
        // SAFETY: the context was created above and is destroyed once.
        unsafe {
            destroy(context);
        }
    }
    result
}

/// The ADL adapter index of the first existing adapter at `location` (or
/// anywhere, without one).
fn find_adapter(api: &AdlApi, context: Context, location: Option<PciLocation>) -> Option<i32> {
    let mut count = 0i32;
    let number_of_adapters = api.number_of_adapters_get?;
    // SAFETY: a live context and a live out parameter.
    if unsafe { number_of_adapters(context, &mut count) } != ADL_OK || count <= 0 {
        return None;
    }
    let mut adapters: Vec<AdapterInfo> = (0..count).map(|_| zeroed()).collect();
    let adapter_info = api.adapter_info_get?;
    let size = std::mem::size_of::<AdapterInfo>() as i32 * count;
    // SAFETY: `adapters` holds `count` structures, `size` bytes in all,
    // which is the input size the call is told.
    if unsafe { adapter_info(context, adapters.as_mut_ptr(), size) } != ADL_OK {
        return None;
    }
    adapters
        .iter()
        .filter(|adapter| adapter.i_exist != 0)
        .find(|adapter| match location {
            Some((bus, device, function)) => {
                adapter.i_bus_number == bus as i32
                    && adapter.i_device_number == device as i32
                    && adapter.i_function_number == function as i32
            }
            None => true,
        })
        .map(|adapter| adapter.i_adapter_index)
}

/// What ADL knows about the card's memory. Every field is optional.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdlMemory {
    pub memory_type: Option<String>,
    pub memory_vendor: Option<String>,
    pub memory_bytes: Option<i64>,
}

/// What ADL knows about the card's memory, at `location` (bus, device,
/// function) when given; empty when ADL cannot say.
pub fn read_memory(location: Option<PciLocation>) -> AdlMemory {
    let query = |api: &AdlApi, context: Context, adapter_index: i32| {
        let memory_info = api.memory_info_x4_get?;
        let mut memory: MemoryInfoX4 = zeroed();
        // SAFETY: a live context, an index ADL listed, and a zeroed
        // ADLMemoryInfoX4 for it to fill.
        if unsafe { memory_info(context, adapter_index, &mut memory) } != ADL_OK {
            return Some(AdlMemory::default());
        }
        let mut found = AdlMemory::default();
        let kind = fixed_text(&memory.str_memory_type);
        if !kind.is_empty() {
            found.memory_type = Some(kind);
        }
        found.memory_vendor = vram_vendor(Some(memory.i_vram_vendor_rev_id));
        if memory.i_memory_size > 0 {
            found.memory_bytes = Some(memory.i_memory_size);
        }
        Some(found)
    };
    with_adapter(location, query).unwrap_or_default()
}

/// A fixed `char[]` field as ctypes returned it -- the bytes before the first
/// NUL -- then `.decode("ascii", "ignore").strip()`.
fn fixed_text(field: &[c_char]) -> String {
    let text: String = field
        .iter()
        .map(|c| *c as u8)
        .take_while(|b| *b != 0)
        .filter(u8::is_ascii)
        .map(char::from)
        .collect();
    py_strip(&text).to_string()
}

/// One PMLog integer as display text, in the unit the row is named for.
pub fn format_sensor(value: i32, unit: &str, scale: f64) -> String {
    let scaled = value as f64 * scale;
    match unit {
        "V" => format!("{scaled:.3} V"),
        "%" => format!("{scaled:.0} %"),
        "RPM" => format!("{scaled:.0} RPM"),
        // Whole watts: ADL hands over an integer, and a decimal point here
        // would claim a precision the driver never gave.
        "W" => format!("{scaled:.0} W"),
        _ => format!("{scaled:.1} {unit}"),
    }
}

/// `{label: display text}` for the card's sensors at `location`, or empty.
///
/// One query serves every row: the whole sensor block comes back in a single
/// call, so nine rows cost one trip through the driver rather than nine.
pub fn read_sensors(location: Option<PciLocation>) -> BTreeMap<&'static str, String> {
    let query = |api: &AdlApi, context: Context, adapter_index: i32| {
        let call = api.new_query_pmlog_data_get?;
        let mut data = PMLogDataOutput { size: 0, sensors: [SensorData::default(); MAX_PMLOG_SENSORS] };
        // SAFETY: a live context, an index ADL listed, and a zeroed
        // ADLPMLogDataOutput for it to fill.
        if unsafe { call(context, adapter_index, &mut data) } != ADL_OK {
            return None;
        }
        let mut found = BTreeMap::new();
        for (index, label, unit, scale) in PMLOG_SENSORS {
            let sensor = data.sensors[*index];
            if sensor.supported != 0 {
                found.insert(*label, format_sensor(sensor.value, unit, *scale));
            }
        }
        Some(found)
    };
    with_adapter(location, query).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    // --- VramVendorTest: only the code this bench answered is named.

    #[test]
    fn the_bench_code_is_named() {
        // ADL answers 1 for this card and GPU-Z, reading the same VBIOS its
        // own way, names the memory Samsung. Two tools, one card, agreeing.
        assert_eq!(vram_vendor(Some(1)).as_deref(), Some("Samsung"));
    }

    #[test]
    fn an_unmeasured_code_prints_as_itself() {
        // The same rule the NVAPI tables follow. A list filled in from
        // documentation nobody checked here is how a table starts being
        // wrong quietly.
        assert_eq!(vram_vendor(Some(6)).as_deref(), Some("Vendor 6"));
    }

    #[test]
    fn nothing_read_is_not_a_vendor() {
        assert_eq!(vram_vendor(None), None);
    }

    // --- ReadMemoryTest

    #[test]
    fn a_machine_without_the_driver_says_nothing() {
        // No Radeon driver, no atiadlxx.dll. Not an error: a machine this
        // module has nothing to say about.
        TEST_ADL.with(|slot| slot.set(None));
        assert_eq!(read_memory(None), AdlMemory::default());
    }

    unsafe extern "C" fn refuse_create(_: MallocCallback, _: i32, _: *mut Context) -> i32 {
        1
    }

    #[test]
    fn a_library_that_will_not_start_says_nothing() {
        TEST_ADL.with(|slot| {
            slot.set(Some(AdlApi { main_control_create: Some(refuse_create), ..AdlApi::default() }))
        });
        assert_eq!(read_memory(None), AdlMemory::default());
    }

    // --- FormatSensorTest: ADL returns integers; the row names the unit.

    #[test]
    fn a_voltage_is_scaled_from_millivolts() {
        assert_eq!(format_sensor(749, "V", 0.001), "0.749 V");
    }

    #[test]
    fn a_fan_reads_as_whole_rpm() {
        assert_eq!(format_sensor(1825, "RPM", 1.0), "1825 RPM");
    }

    #[test]
    fn a_percentage_has_no_decimals() {
        assert_eq!(format_sensor(50, "%", 1.0), "50 %");
    }

    #[test]
    fn a_temperature_keeps_one() {
        assert_eq!(format_sensor(32, "\u{00b0}C", 1.0), "32.0 °C");
    }

    // --- PmlogSensorTest

    #[test]
    fn the_four_exactly_matched_sensors_are_present() {
        // Fan, fan PWM, utilisation and memory clock each matched HWiNFO to
        // the digit, which is what confirms the index enumeration rather than
        // merely making it plausible.
        for index in [14, 15, 19, 2] {
            assert!(PMLOG_SENSORS.iter().any(|(i, ..)| *i == index), "index {index}");
        }
    }

    #[test]
    fn board_power_is_present_and_measured_in_watts() {
        // Index 73 was written off as unreachable before it was ever printed.
        // A GPU load found it: 37 W at rest, pinned at 361-371 W under full
        // utilisation, back to 38 W after. A limit would not have moved.
        let entry = PMLOG_SENSORS.iter().find(|(index, ..)| *index == 73);
        assert_eq!(entry.map(|(_, _, unit, _)| *unit), Some("W"));
    }

    #[test]
    fn watts_are_whole() {
        // ADL hands over an integer; a decimal would invent precision.
        assert_eq!(format_sensor(363, "W", 1.0), "363 W");
    }

    #[test]
    fn every_sensor_has_a_unit_the_formatter_knows() {
        for (_index, label, unit, scale) in PMLOG_SENSORS {
            assert!(["V", "%", "RPM", "MHz", "W", "\u{00b0}C"].contains(unit), "{label}");
            assert!(*scale > 0.0, "{label}");
        }
    }

    #[test]
    fn a_machine_without_the_driver_reports_no_sensors() {
        TEST_ADL.with(|slot| slot.set(None));
        assert!(read_sensors(None).is_empty());
    }

    // --- Beyond the Python tests: a stand-in atiadlxx.dll with two outputs
    // of one card at 03:00.0, so the adapter walk, the structure layouts and
    // the always-destroy rule are exercised without the driver.

    thread_local! {
        static DESTROYED: Cell<u32> = const { Cell::new(0) };
        static ASKED: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    }

    const FAKE_CONTEXT: usize = 0xADC0;

    unsafe extern "C" fn fake_create(malloc: MallocCallback, connected: i32, context: *mut Context) -> i32 {
        assert_eq!(connected, 1);
        // SAFETY: the callback is ours; ADL would use it the same way.
        let buffer = unsafe { malloc(64) };
        assert!(!buffer.is_null());
        // SAFETY: the out parameter is the caller's live local.
        unsafe { *context = FAKE_CONTEXT as Context };
        0
    }

    unsafe extern "C" fn fake_destroy(context: Context) -> i32 {
        assert_eq!(context as usize, FAKE_CONTEXT);
        DESTROYED.with(|d| d.set(d.get() + 1));
        0
    }

    unsafe extern "C" fn fake_count(_: Context, count: *mut i32) -> i32 {
        // SAFETY: the caller's live out parameter.
        unsafe { *count = 3 };
        0
    }

    unsafe extern "C" fn fake_info(_: Context, adapters: *mut AdapterInfo, size: i32) -> i32 {
        assert_eq!(size, 3 * 1572);
        // SAFETY: the caller passes three zeroed structures.
        let adapters = unsafe { std::slice::from_raw_parts_mut(adapters, 3) };
        // A listed but absent output first, then two outputs of the card.
        for (slot, (index, exists)) in adapters.iter_mut().zip([(4, 0), (7, 1), (8, 1)]) {
            slot.i_adapter_index = index;
            slot.i_exist = exists;
            slot.i_bus_number = 3;
        }
        0
    }

    unsafe extern "C" fn fake_memory(_: Context, index: i32, memory: *mut MemoryInfoX4) -> i32 {
        ASKED.with(|asked| asked.borrow_mut().push(index));
        // SAFETY: the caller's zeroed structure.
        let memory = unsafe { &mut *memory };
        for (slot, byte) in memory.str_memory_type.iter_mut().zip(b"GDDR6 ") {
            *slot = *byte as c_char;
        }
        memory.i_vram_vendor_rev_id = 1;
        memory.i_memory_size = 17_095_983_104;
        0
    }

    unsafe extern "C" fn fake_pmlog(_: Context, _: i32, data: *mut PMLogDataOutput) -> i32 {
        // SAFETY: the caller's zeroed structure.
        let data = unsafe { &mut *data };
        data.sensors[14] = SensorData { supported: 1, value: 1825 };
        data.sensors[21] = SensorData { supported: 1, value: 749 };
        data.sensors[73] = SensorData { supported: 1, value: 363 };
        data.sensors[8] = SensorData { supported: 0, value: 31 };
        0
    }

    fn fake_driver() {
        DESTROYED.with(|d| d.set(0));
        TEST_ADL.with(|slot| {
            slot.set(Some(AdlApi {
                main_control_create: Some(fake_create),
                main_control_destroy: Some(fake_destroy),
                number_of_adapters_get: Some(fake_count),
                adapter_info_get: Some(fake_info),
                memory_info_x4_get: Some(fake_memory),
                new_query_pmlog_data_get: Some(fake_pmlog),
            }))
        });
    }

    #[test]
    fn the_first_existing_output_at_the_location_is_asked() {
        fake_driver();
        let memory = read_memory(Some((3, 0, 0)));
        assert_eq!(
            memory,
            AdlMemory {
                memory_type: Some("GDDR6".into()),
                memory_vendor: Some("Samsung".into()),
                memory_bytes: Some(17_095_983_104),
            }
        );
        assert_eq!(ASKED.with(|asked| asked.borrow().clone()), vec![7]);
        assert_eq!(DESTROYED.with(Cell::get), 1);
    }

    #[test]
    fn another_location_finds_nothing_and_still_closes() {
        fake_driver();
        assert_eq!(read_memory(Some((4, 0, 0))), AdlMemory::default());
        assert_eq!(DESTROYED.with(Cell::get), 1);
    }

    #[test]
    fn one_query_serves_every_supported_sensor() {
        fake_driver();
        let sensors = read_sensors(None);
        assert_eq!(sensors.len(), 3);
        assert_eq!(sensors["GPU Fan"], "1825 RPM");
        assert_eq!(sensors["GPU Core Voltage"], "0.749 V");
        assert_eq!(sensors["GPU Board Power (TBP)"], "363 W");
        assert!(!sensors.contains_key("GPU Temperature"));
    }
}
