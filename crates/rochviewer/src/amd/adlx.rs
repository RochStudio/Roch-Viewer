//! The card's board power limit, read through ADLX.
//!
//! ADLX is AMD's newer library, and it is a C++ object model behind a C shim
//! rather than the flat exports of atiadlxx. Every interface is a struct whose
//! first member points at a vtable, and calling a method means indexing a slot.
//! A wrong slot does not return a wrong number: it calls whatever function
//! happens to sit there. So the slot constants below were transcribed from the
//! SDK headers -- SDK/Include/ISystem.h and IPerformanceMonitoring.h in
//! GPUOpen-LibrariesAndSDKs/ADLX -- rather than recalled, and each block is
//! counted from its own declaration.
//!
//! Only one reading is taken here, because only one is new. Everything else ADLX
//! offers on this card, atiadlxx already answers, and two sources for one row is
//! a way to disagree with yourself. See [`crate::amd::adl`] for those.
//!
//! What ADLX does *not* have, checked rather than assumed: there is no fabric
//! clock anywhere in its headers, and no effective-clock metric. GPU FCLK is not
//! reachable through this library either.
//!
//! Read-only. The tuning services -- the ones that could change a limit rather
//! than report it -- are never asked for.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::time::Instant;

use crate::memo::Memo;

pub const ADLX_OK: i32 = 0;
/// IADLXSystem is the one interface that is not reference-counted, so its
/// vtable starts at its own methods rather than Acquire/Release/QueryInterface.
pub const SYSTEM_GET_GPUS: usize = 1;
pub const SYSTEM_GET_PERF_SERVICES: usize = 9;

/// Every other interface begins with Acquire/Release/QueryInterface, which is
/// why the useful slots start at 3 and why Release is always 1.
pub const RELEASE: usize = 1;
pub const LIST_SIZE: usize = 3;
pub const LIST_BEGIN: usize = 5;
pub const LIST_AT_GPULIST: usize = 11;
pub const PERF_GET_SUPPORTED_GPU_METRICS: usize = 21;
pub const SUPPORT_TOTAL_BOARD_POWER_RANGE: usize = 22;

/// The limit moves only when somebody drags the slider in Adrenalin, so it is
/// not worth a driver round trip every tick. Thirty seconds still notices.
pub const CACHE_SECONDS: f64 = 30.0;

/// (when it was read, what was read).
static CACHE: Memo<(f64, Option<i32>)> = Memo::new();

type Object = *mut c_void;

// The method shapes, each called through a vtable slot. ADLX methods are
// __stdcall and the shim's exports __cdecl; on x64 both are the one
// convention there is, which is what the Python's CFUNCTYPE relied on.
type GetObjectFn = unsafe extern "C" fn(Object, *mut Object) -> i32;
type ListCountFn = unsafe extern "C" fn(Object) -> u32;
type ListAtFn = unsafe extern "C" fn(Object, u32, *mut Object) -> i32;
type SupportedMetricsFn = unsafe extern "C" fn(Object, Object, *mut Object) -> i32;
type RangeFn = unsafe extern "C" fn(Object, *mut i32, *mut i32) -> i32;
type ReleaseFn = unsafe extern "C" fn(Object) -> i32;

type QueryFullVersionFn = unsafe extern "C" fn(*mut u64) -> i32;
type InitializeFn = unsafe extern "C" fn(u64, *mut Object) -> i32;
type TerminateFn = unsafe extern "C" fn() -> i32;

/// The exports of amdadlx64.dll this module calls; `None` for one it lacks.
#[derive(Clone, Copy, Default)]
struct AdlxApi {
    query_full_version: Option<QueryFullVersionFn>,
    initialize: Option<InitializeFn>,
    terminate: Option<TerminateFn>,
}

#[cfg(test)]
thread_local! {
    /// The exports a test stands in for amdadlx64.dll.
    static TEST_ADLX: std::cell::Cell<Option<AdlxApi>> = const { std::cell::Cell::new(None) };
}

fn load_adlx() -> Option<AdlxApi> {
    #[cfg(test)]
    {
        // Unit tests never load a vendor library.
        TEST_ADLX.with(|slot| slot.get())
    }
    #[cfg(not(test))]
    {
        use libloading::os::windows::{LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, Library};

        static LOADED: std::sync::Mutex<Option<AdlxApi>> = std::sync::Mutex::new(None);
        let mut loaded = LOADED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(api) = *loaded {
            return Some(api);
        }

        // SAFETY: loading the driver's library runs its initialisation, as
        // loading it through ctypes did; the default search directories are
        // the ones ctypes uses for a bare name.
        let library = unsafe { Library::load_with_flags("amdadlx64.dll", LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) }.ok()?;
        fn export<T: Copy>(library: &Library, name: &str) -> Option<T> {
            // SAFETY: each export is named with its ADLX SDK signature.
            unsafe { library.get::<T>(name) }.ok().map(|found| *found)
        }
        let api = AdlxApi {
            query_full_version: export(&library, "ADLXQueryFullVersion"),
            initialize: export(&library, "ADLXInitialize"),
            terminate: export(&library, "ADLXTerminate"),
        };
        // Kept loaded for the life of the process, as ctypes keeps every
        // library it loads; unloading the driver's library once a tick is
        // not something the Python ever did.
        std::mem::forget(library);
        *loaded = Some(api);
        Some(api)
    }
}

/// The function pointer at `slot` of `obj`'s vtable, as the type `F`.
///
/// # Safety
///
/// `obj` must be a live ADLX interface whose vtable has a method of shape `F`
/// at `slot`.
unsafe fn method<F: Copy>(obj: Object, slot: usize) -> F {
    const { assert!(std::mem::size_of::<F>() == std::mem::size_of::<usize>()) };
    // SAFETY: an ADLX interface's first member is its vtable pointer, and the
    // caller vouches for the slot and its shape.
    unsafe {
        let vtable = *(obj as *const *const usize);
        let address = *vtable.add(slot);
        std::mem::transmute_copy::<usize, F>(&address)
    }
}

fn release(obj: Object) {
    if !obj.is_null() {
        // SAFETY: every object released here is a reference-counted ADLX
        // interface that this module acquired, and Release is slot 1 of each.
        unsafe {
            let call: ReleaseFn = method(obj, RELEASE);
            call(obj);
        }
    }
}

/// Open ADLX, find the first card and the metrics services, always close.
///
/// Everything acquired is released afterwards, and ADLX is terminated there
/// too. ADLX counts its objects and complains about orphans, and a failed
/// read that leaked would otherwise do it once a tick.
fn with_gpu<T>(work: impl FnOnce(Object) -> Option<T>) -> Option<T> {
    let api = load_adlx()?;

    let mut version = 0u64;
    let query_full_version = api.query_full_version?;
    // SAFETY: the out parameter is a live local.
    if unsafe { query_full_version(&mut version) } != ADLX_OK {
        return None;
    }
    let mut system: Object = std::ptr::null_mut();
    let initialize = api.initialize?;
    // SAFETY: the version ADLX itself reported, and a live out parameter for
    // the IADLXSystem pointer.
    if unsafe { initialize(version, &mut system) } != ADLX_OK || system.is_null() {
        return None;
    }

    let mut gpu_list: Object = std::ptr::null_mut();
    let mut gpu: Object = std::ptr::null_mut();
    let mut perf: Object = std::ptr::null_mut();
    let mut support: Object = std::ptr::null_mut();
    let result = (|| {
        // SAFETY (each call below): `system` is the IADLXSystem ADLX handed
        // out, and each later object is one a call on it returned, checked
        // by its status and checked non-null before a method is called
        // through it; the slots are those of the SDK headers, and every out
        // parameter is a live local.
        unsafe {
            let get_gpus: GetObjectFn = method(system, SYSTEM_GET_GPUS);
            // A null object behind ADLX_OK is a failed read, not something
            // to call through: the Python's ctypes raised "NULL pointer
            // access" there and the read came back None.
            if get_gpus(system, &mut gpu_list) != ADLX_OK || gpu_list.is_null() {
                return None;
            }
            let size: ListCountFn = method(gpu_list, LIST_SIZE);
            if size(gpu_list) == 0 {
                return None;
            }
            let begin: ListCountFn = method(gpu_list, LIST_BEGIN);
            let first = begin(gpu_list);
            let at: ListAtFn = method(gpu_list, LIST_AT_GPULIST);
            if at(gpu_list, first, &mut gpu) != ADLX_OK {
                return None;
            }
            let get_perf: GetObjectFn = method(system, SYSTEM_GET_PERF_SERVICES);
            if get_perf(system, &mut perf) != ADLX_OK || perf.is_null() {
                return None;
            }
            let supported: SupportedMetricsFn = method(perf, PERF_GET_SUPPORTED_GPU_METRICS);
            if supported(perf, gpu, &mut support) != ADLX_OK || support.is_null() {
                return None;
            }
        }
        work(support)
    })();

    for handle in [support, perf, gpu, gpu_list] {
        release(handle);
    }
    if let Some(terminate) = api.terminate {
        // SAFETY: paired with the successful ADLXInitialize above.
        unsafe {
            terminate();
        }
    }
    result
}

/// The card's board power ceiling in watts, or `None`.
///
/// ADLX reports it as the top of the board power range, which is the same
/// 363 W the card pins itself to under a sustained load.
pub fn board_power_limit() -> Option<i32> {
    with_gpu(|support| {
        let (mut low, mut high) = (0i32, 0i32);
        // SAFETY: `support` is the IADLXGPUMetricsSupport ADLX returned, whose
        // slot 22 is GetGPUTotalBoardPowerRange(int*, int*).
        let status = unsafe {
            let range: RangeFn = method(support, SUPPORT_TOTAL_BOARD_POWER_RANGE);
            range(support, &mut low, &mut high)
        };
        if status != ADLX_OK || high <= 0 {
            return None;
        }
        Some(high)
    })
}

/// `time.monotonic()`: seconds on a clock that only moves forward.
fn monotonic() -> f64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// The limit, re-read at most every [`CACHE_SECONDS`]. `now` is the
/// monotonic time in seconds; `None` reads the clock.
pub fn cached_limit(now: Option<f64>) -> Option<i32> {
    cached_limit_with(now, board_power_limit)
}

fn cached_limit_with(now: Option<f64>, read: impl FnOnce() -> Option<i32>) -> Option<i32> {
    let now = now.unwrap_or_else(monotonic);
    if let Some((when, value)) = CACHE.get() {
        if now - when < CACHE_SECONDS {
            return value;
        }
    }
    let value = read();
    CACHE.set((now, value));
    value
}

/// What [`limit_text`] is to show.
///
/// "Not supplied" and "ADLX would not answer" are both absences and they mean
/// opposite things: one says go and read, the other says say nothing. The same
/// confusion cost a test in whea_errors, so it is spelled out here too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watts {
    /// Not supplied: go and read it.
    Unread,
    /// Read: whole watts, or `None` when ADLX did not answer.
    Read(Option<i32>),
}

/// The row's text: whole watts, or an em dash when ADLX did not answer.
pub fn limit_text(watts: Watts) -> String {
    let watts = match watts {
        Watts::Unread => cached_limit(None),
        Watts::Read(watts) => watts,
    };
    match watts {
        None => "—".into(),
        Some(watts) => format!("{watts} W"),
    }
}

#[cfg(test)]
mod tests {
    //! ADLX reads a limit atiadlxx has no answer for, and fails closed
    //! without it.

    use super::*;
    use std::cell::{Cell, RefCell};

    // --- LimitTextTest

    #[test]
    fn whole_watts() {
        assert_eq!(limit_text(Watts::Read(Some(363))), "363 W");
    }

    #[test]
    fn an_unreadable_limit_says_nothing() {
        // An em dash rather than a zero: zero is a claim about the card, and
        // a library that would not answer has not made one.
        assert_eq!(limit_text(Watts::Read(None)), "—");
    }

    #[test]
    fn not_supplied_reads_rather_than_reporting_nothing() {
        // The sentinel is the whole point: None means unreadable, and a
        // missing argument means go and look. The look finds a fresh 363 W
        // in the cache, as the patched cached_limit returned.
        CACHE.set((monotonic(), Some(363)));
        assert_eq!(limit_text(Watts::Unread), "363 W");
    }

    // --- CacheTest

    #[test]
    fn the_limit_is_not_re_read_every_tick() {
        let calls = Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            Some(363)
        };
        assert_eq!(cached_limit_with(Some(100.0), read), Some(363));
        assert_eq!(cached_limit_with(Some(110.0), read), Some(363));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_stale_limit_is_re_read() {
        let answers = RefCell::new(vec![363, 300].into_iter());
        let calls = Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            answers.borrow_mut().next()
        };
        assert_eq!(cached_limit_with(Some(100.0), read), Some(363));
        let later = 100.0 + CACHE_SECONDS + 1.0;
        assert_eq!(cached_limit_with(Some(later), read), Some(300));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn an_unreadable_limit_is_cached_too() {
        // Otherwise a machine without the library pays a failed load every
        // tick, which is the cost the cache exists to avoid.
        let calls = Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            None
        };
        assert_eq!(cached_limit_with(Some(100.0), read), None);
        assert_eq!(cached_limit_with(Some(101.0), read), None);
        assert_eq!(calls.get(), 1);
    }

    // --- NoLibraryTest

    #[test]
    fn a_machine_without_adlx_reports_no_limit() {
        TEST_ADLX.with(|slot| slot.set(None));
        assert_eq!(board_power_limit(), None);
    }

    // --- SlotConstantsTest: the slots are the part that cannot be wrong
    // quietly. A wrong index calls whatever function sits at it, so these are
    // pinned to what the SDK headers declare rather than left to drift.

    #[test]
    fn release_is_the_second_slot_of_every_counted_interface() {
        assert_eq!(RELEASE, 1);
    }

    #[test]
    fn system_slots_skip_no_reference_counting() {
        // IADLXSystem is the one interface that is not reference-counted, so
        // its methods start at slot 0 rather than after Acquire/Release/QI.
        assert_eq!(SYSTEM_GET_GPUS, 1);
        assert_eq!(SYSTEM_GET_PERF_SERVICES, 9);
    }

    #[test]
    fn the_counted_interfaces_start_after_the_first_three() {
        for slot in [
            LIST_SIZE,
            LIST_BEGIN,
            LIST_AT_GPULIST,
            PERF_GET_SUPPORTED_GPU_METRICS,
            SUPPORT_TOTAL_BOARD_POWER_RANGE,
        ] {
            assert!(slot >= 3, "slot {slot}");
        }
    }

    #[test]
    fn the_board_power_range_slot_is_where_the_header_puts_it() {
        assert_eq!(SUPPORT_TOTAL_BOARD_POWER_RANGE, 22);
    }

    // --- ReadOnlyTest. The Python listed the module's names with dir(); here
    // the module's own source, above its tests, is what declares names.

    /// The identifiers this module declares (const, static, fn, type,
    /// struct, enum), from its source above the tests.
    fn declared_names() -> Vec<String> {
        let source = include_str!("adlx.rs");
        let body = &source[..source.find("\nmod tests {").unwrap_or(source.len())];
        let mut names = Vec::new();
        for line in body.lines() {
            let line = line.trim_start().trim_start_matches("pub ").trim_start_matches("unsafe ");
            for keyword in ["const ", "static ", "fn ", "type ", "struct ", "enum "] {
                if let Some(rest) = line.strip_prefix(keyword) {
                    let name: String =
                        rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
        }
        names
    }

    #[test]
    fn no_tuning_slot_is_defined() {
        // GetGPUTuningServices is slot 8 of IADLXSystem and is the door to
        // changing a limit rather than reporting it. Not naming the slot is
        // what keeps that door shut: the module cannot call what it has no
        // constant for.
        let names = declared_names();
        assert!(names.iter().any(|name| name == "SYSTEM_GET_GPUS"), "the scan finds names");
        let named: Vec<_> = names.iter().filter(|name| name.to_uppercase().contains("TUNING")).collect();
        assert!(named.is_empty(), "{named:?}");
    }

    #[test]
    fn the_only_system_slots_named_are_the_two_readers() {
        let mut slots: Vec<_> =
            declared_names().into_iter().filter(|name| name.starts_with("SYSTEM_")).collect();
        slots.sort();
        assert_eq!(slots, ["SYSTEM_GET_GPUS", "SYSTEM_GET_PERF_SERVICES"]);
    }

    // --- Beyond the Python tests: a stand-in ADLX whose objects carry vtables
    // with methods only at the slots the constants name, so a wrong slot
    // calls a null and the test dies rather than passing.

    #[repr(C)]
    struct FakeObject {
        vtable: *const usize,
    }

    thread_local! {
        static RELEASED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
        static TERMINATED: Cell<bool> = const { Cell::new(false) };
        static SYSTEM: Cell<usize> = const { Cell::new(0) };
        static OBJECTS: RefCell<Vec<(&'static str, usize)>> = const { RefCell::new(Vec::new()) };
    }

    fn object(name: &'static str, slots: &[(usize, usize)]) -> Object {
        let mut table = vec![0usize; 32];
        for (slot, address) in slots {
            table[*slot] = *address;
        }
        let vtable = Box::leak(table.into_boxed_slice()).as_ptr();
        let obj = Box::leak(Box::new(FakeObject { vtable })) as *mut FakeObject as Object;
        OBJECTS.with(|objects| objects.borrow_mut().push((name, obj as usize)));
        obj
    }

    fn named_object(name: &str) -> Object {
        OBJECTS.with(|objects| {
            objects.borrow().iter().find(|(n, _)| *n == name).map(|(_, a)| *a as Object).unwrap()
        })
    }

    unsafe extern "C" fn fake_release(obj: Object) -> i32 {
        let name = OBJECTS.with(|objects| {
            objects.borrow().iter().find(|(_, a)| *a == obj as usize).map(|(n, _)| *n).unwrap()
        });
        RELEASED.with(|released| released.borrow_mut().push(name));
        0
    }

    thread_local! {
        /// The one object the fake hands back as null while saying ADLX_OK.
        static NULL_OBJECT: Cell<&'static str> = const { Cell::new("") };
    }

    /// `name`'s object, or null when the test asked for that one to be null.
    fn handed_out(name: &'static str) -> Object {
        if NULL_OBJECT.with(Cell::get) == name { std::ptr::null_mut() } else { named_object(name) }
    }

    unsafe extern "C" fn fake_get_gpus(_: Object, out: *mut Object) -> i32 {
        // SAFETY: the caller's live out parameter.
        unsafe { *out = handed_out("gpu_list") };
        0
    }

    unsafe extern "C" fn fake_get_perf(_: Object, out: *mut Object) -> i32 {
        // SAFETY: as above.
        unsafe { *out = handed_out("perf") };
        0
    }

    unsafe extern "C" fn fake_size(_: Object) -> u32 {
        1
    }

    unsafe extern "C" fn fake_begin(_: Object) -> u32 {
        0
    }

    unsafe extern "C" fn fake_at(_: Object, index: u32, out: *mut Object) -> i32 {
        assert_eq!(index, 0);
        // SAFETY: as above.
        unsafe { *out = named_object("gpu") };
        0
    }

    unsafe extern "C" fn fake_supported(_: Object, gpu: Object, out: *mut Object) -> i32 {
        assert_eq!(gpu, named_object("gpu"));
        // SAFETY: as above.
        unsafe { *out = handed_out("support") };
        0
    }

    unsafe extern "C" fn fake_range(_: Object, low: *mut i32, high: *mut i32) -> i32 {
        // SAFETY: the caller's live out parameters.
        unsafe {
            *low = 30;
            *high = 363;
        }
        0
    }

    unsafe extern "C" fn fake_query_full_version(version: *mut u64) -> i32 {
        // SAFETY: as above.
        unsafe { *version = 0x0001_0004 };
        0
    }

    unsafe extern "C" fn fake_initialize(version: u64, system: *mut Object) -> i32 {
        assert_eq!(version, 0x0001_0004);
        // SAFETY: as above.
        unsafe { *system = SYSTEM.with(Cell::get) as Object };
        0
    }

    unsafe extern "C" fn fake_terminate() -> i32 {
        TERMINATED.with(|t| t.set(true));
        0
    }

    /// The fake ADLX the two tests below read through.
    fn install_fake_adlx() {
        let release = fake_release as ReleaseFn as usize;
        let system = object(
            "system",
            &[
                (SYSTEM_GET_GPUS, fake_get_gpus as GetObjectFn as usize),
                (SYSTEM_GET_PERF_SERVICES, fake_get_perf as GetObjectFn as usize),
            ],
        );
        object(
            "gpu_list",
            &[
                (RELEASE, release),
                (LIST_SIZE, fake_size as ListCountFn as usize),
                (LIST_BEGIN, fake_begin as ListCountFn as usize),
                (LIST_AT_GPULIST, fake_at as ListAtFn as usize),
            ],
        );
        object("gpu", &[(RELEASE, release)]);
        object(
            "perf",
            &[(RELEASE, release), (PERF_GET_SUPPORTED_GPU_METRICS, fake_supported as SupportedMetricsFn as usize)],
        );
        object(
            "support",
            &[(RELEASE, release), (SUPPORT_TOTAL_BOARD_POWER_RANGE, fake_range as RangeFn as usize)],
        );
        SYSTEM.with(|s| s.set(system as usize));
        TEST_ADLX.with(|slot| {
            slot.set(Some(AdlxApi {
                query_full_version: Some(fake_query_full_version),
                initialize: Some(fake_initialize),
                terminate: Some(fake_terminate),
            }))
        });
    }

    #[test]
    fn the_limit_is_read_through_the_header_slots_and_everything_is_released() {
        install_fake_adlx();
        assert_eq!(board_power_limit(), Some(363));
        assert_eq!(
            RELEASED.with(|released| released.borrow().clone()),
            ["support", "perf", "gpu", "gpu_list"]
        );
        assert!(TERMINATED.with(Cell::get));
    }

    #[test]
    fn a_null_object_behind_adlx_ok_reads_nothing_and_still_closes() {
        // The Python's ctypes raised "NULL pointer access" calling through a
        // null interface, and _with_gpu turned that into None. Calling
        // through it here would read address zero.
        for name in ["gpu_list", "perf", "support"] {
            RELEASED.with(|released| released.borrow_mut().clear());
            TERMINATED.with(|t| t.set(false));
            NULL_OBJECT.with(|null| null.set(name));
            install_fake_adlx();
            assert_eq!(board_power_limit(), None, "{name} null");
            assert!(TERMINATED.with(Cell::get), "{name} null: ADLX was not terminated");
            assert!(!RELEASED.with(|released| released.borrow().contains(&name)), "{name} null was released");
        }
    }
}
