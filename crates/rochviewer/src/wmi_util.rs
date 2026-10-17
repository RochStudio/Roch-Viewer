//! WMI access, one connection per thread, with a seam for tests.
//!
//! The Python called `wmi.WMI().Win32_Processor()` and read attributes off
//! the returned objects with `getattr(obj, "Name", "")`. Here a query returns
//! [`WmiObject`]s, a field map with the same forgiving accessors: a missing
//! field and a null field both read as `None`, and numbers that WMI hands
//! back as strings (every `uint64`, such as `Capacity`) parse as numbers.
//!
//! COM ties a connection to the thread that made it, so the real source
//! keeps one connection on a dedicated thread and every query is run there.
//! Tests replace the source for their own thread with
//! [`set_thread_source`], which is how the Python tests' `FakeWMI` is ported.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::memo::MemoMap;

#[derive(Clone, Debug, PartialEq)]
pub enum WmiValue {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Str(String),
    Array(Vec<WmiValue>),
}

impl WmiValue {
    /// Python's `str(value)` for the shapes WMI returns.
    pub fn to_text(&self) -> Option<String> {
        match self {
            WmiValue::Null => None,
            WmiValue::Bool(b) => Some(if *b { "True".into() } else { "False".into() }),
            WmiValue::Int(i) => Some(i.to_string()),
            WmiValue::UInt(u) => Some(u.to_string()),
            WmiValue::Float(f) => Some(crate::pyfmt::float_str(*f)),
            WmiValue::Str(s) => Some(s.clone()),
            WmiValue::Array(items) => Some(format!(
                "({})",
                items
                    .iter()
                    .map(|v| v.to_text().unwrap_or_else(|| "None".into()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// Python's `int(value)`: integers, whole floats and numeric strings.
    pub fn to_i64(&self) -> Option<i64> {
        match self {
            WmiValue::Int(i) => Some(*i),
            WmiValue::UInt(u) => i64::try_from(*u).ok(),
            WmiValue::Float(f) if f.is_finite() => Some(f.trunc() as i64),
            WmiValue::Bool(b) => Some(*b as i64),
            WmiValue::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn to_u64(&self) -> Option<u64> {
        match self {
            WmiValue::UInt(u) => Some(*u),
            WmiValue::Str(s) => s.trim().parse().ok(),
            other => other.to_i64().and_then(|i| u64::try_from(i).ok()),
        }
    }

    pub fn to_f64(&self) -> Option<f64> {
        match self {
            WmiValue::Float(f) => Some(*f),
            WmiValue::Int(i) => Some(*i as f64),
            WmiValue::UInt(u) => Some(*u as f64),
            WmiValue::Str(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

impl From<&str> for WmiValue {
    fn from(value: &str) -> Self {
        WmiValue::Str(value.to_string())
    }
}
impl From<String> for WmiValue {
    fn from(value: String) -> Self {
        WmiValue::Str(value)
    }
}
impl From<i64> for WmiValue {
    fn from(value: i64) -> Self {
        WmiValue::Int(value)
    }
}
impl From<i32> for WmiValue {
    fn from(value: i32) -> Self {
        WmiValue::Int(value as i64)
    }
}
impl From<u64> for WmiValue {
    fn from(value: u64) -> Self {
        WmiValue::UInt(value)
    }
}
impl From<u32> for WmiValue {
    fn from(value: u32) -> Self {
        WmiValue::UInt(value as u64)
    }
}
impl From<f64> for WmiValue {
    fn from(value: f64) -> Self {
        WmiValue::Float(value)
    }
}
impl From<bool> for WmiValue {
    fn from(value: bool) -> Self {
        WmiValue::Bool(value)
    }
}

/// One WMI instance: its properties by name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WmiObject {
    pub fields: BTreeMap<String, WmiValue>,
}

impl WmiObject {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder for tests: `WmiObject::new().with("Name", "i9-14900KF")`.
    pub fn with(mut self, field: &str, value: impl Into<WmiValue>) -> Self {
        self.fields.insert(field.to_string(), value.into());
        self
    }

    /// The raw value, or `None` when the property is absent. WMI property
    /// names are case-insensitive, so a differently-cased match is accepted.
    pub fn get(&self, field: &str) -> Option<&WmiValue> {
        self.fields.get(field).or_else(|| {
            self.fields
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(field))
                .map(|(_, value)| value)
        })
    }

    /// `getattr(obj, field, None)` as text; `None` for absent or null.
    pub fn text(&self, field: &str) -> Option<String> {
        self.get(field).and_then(WmiValue::to_text)
    }

    /// `(getattr(obj, field, "") or "")` -- never `None`.
    pub fn str_or_empty(&self, field: &str) -> String {
        self.text(field).unwrap_or_default()
    }

    pub fn int(&self, field: &str) -> Option<i64> {
        self.get(field).and_then(WmiValue::to_i64)
    }

    pub fn uint(&self, field: &str) -> Option<u64> {
        self.get(field).and_then(WmiValue::to_u64)
    }

    pub fn float(&self, field: &str) -> Option<f64> {
        self.get(field).and_then(WmiValue::to_f64)
    }
}

/// Where query results come from. The real one talks to WMI; tests supply
/// their own.
pub trait WmiSource: Send + Sync {
    fn query(&self, class_name: &str) -> Result<Vec<WmiObject>, String>;
}

thread_local! {
    static THREAD_SOURCE: RefCell<Option<Arc<dyn WmiSource>>> = const { RefCell::new(None) };
}

/// Replace the WMI source for the calling thread only. Pass `None` to go back
/// to the real one. Intended for tests.
pub fn set_thread_source(source: Option<Arc<dyn WmiSource>>) {
    THREAD_SOURCE.with(|slot| *slot.borrow_mut() = source);
}

fn thread_source() -> Option<Arc<dyn WmiSource>> {
    THREAD_SOURCE.with(|slot| slot.borrow().clone())
}

/// Every instance of `class_name`, queried now.
pub fn query(class_name: &str) -> Result<Vec<WmiObject>, String> {
    if let Some(source) = thread_source() {
        return source.query(class_name);
    }
    if cfg!(test) {
        // A unit test that forgot to stub WMI must not depend on the machine
        // it runs on.
        return Err("WMI is not stubbed in this test".into());
    }
    real::query(class_name)
}

/// The first instance of `class_name`, queried now.
pub fn query_first(class_name: &str) -> Option<WmiObject> {
    query(class_name).ok().and_then(|mut all| {
        if all.is_empty() { None } else { Some(all.remove(0)) }
    })
}

static STATIC_CLASSES: MemoMap<String, Arc<Vec<WmiObject>>> = MemoMap::new();

/// A WMI class's objects, queried once for the process.
///
/// Only for classes describing identity or installed hardware, which cannot
/// change while the machine is running. Failure is cached too, deliberately:
/// WMI being unavailable is not a transient condition, and retrying it once
/// per caller is how the startup cost multiplied in the first place.
pub fn static_class(class_name: &str) -> Arc<Vec<WmiObject>> {
    STATIC_CLASSES.get_or_init(class_name.to_string(), || match query(class_name) {
        Ok(objects) => Arc::new(objects),
        Err(error) => {
            eprintln!("Error querying {class_name}: {error}");
            Arc::new(Vec::new())
        }
    })
}

/// Forget every cached static class (for the calling test thread, under test).
pub fn clear_static_cache() {
    STATIC_CLASSES.clear();
}

mod real {
    use super::{WmiObject, WmiValue};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, mpsc};
    use wmi::{Variant, WMIConnection};

    fn convert(value: Variant) -> WmiValue {
        match value {
            Variant::Empty | Variant::Null => WmiValue::Null,
            Variant::String(s) => WmiValue::Str(s),
            Variant::I1(v) => WmiValue::Int(v as i64),
            Variant::I2(v) => WmiValue::Int(v as i64),
            Variant::I4(v) => WmiValue::Int(v as i64),
            Variant::I8(v) => WmiValue::Int(v),
            Variant::R4(v) => WmiValue::Float(v as f64),
            Variant::R8(v) => WmiValue::Float(v),
            Variant::Bool(v) => WmiValue::Bool(v),
            Variant::UI1(v) => WmiValue::UInt(v as u64),
            Variant::UI2(v) => WmiValue::UInt(v as u64),
            Variant::UI4(v) => WmiValue::UInt(v as u64),
            Variant::UI8(v) => WmiValue::UInt(v),
            Variant::Array(items) => WmiValue::Array(items.into_iter().map(convert).collect()),
            _ => WmiValue::Null,
        }
    }

    type Reply = mpsc::Sender<Result<Vec<WmiObject>, String>>;

    /// WMI runs on one thread of its own. COM fixes a thread's apartment the
    /// first time it is initialised there, and the window's thread is set up
    /// single-threaded by the windowing library, where the WMI connection
    /// refuses to work (RPC_E_WRONG_THREAD). A dedicated thread owns the one
    /// connection for the process, whichever thread asks.
    fn worker() -> &'static Mutex<mpsc::Sender<(String, Reply)>> {
        static WORKER: OnceLock<Mutex<mpsc::Sender<(String, Reply)>>> = OnceLock::new();
        WORKER.get_or_init(|| {
            let (sender, receiver) = mpsc::channel::<(String, Reply)>();
            std::thread::Builder::new()
                .name("wmi".into())
                .spawn(move || {
                    let mut connection: Option<WMIConnection> = None;
                    for (class_name, reply) in receiver {
                        let _ = reply.send(run(&mut connection, &class_name));
                    }
                })
                .expect("spawning the WMI thread");
            Mutex::new(sender)
        })
    }

    fn run(connection: &mut Option<WMIConnection>, class_name: &str) -> Result<Vec<WmiObject>, String> {
        let timing = std::env::var_os("ROCHVIEWER_TIMING").is_some();
        let started = std::time::Instant::now();
        if connection.is_none() {
            *connection = Some(WMIConnection::new().map_err(|e| e.to_string())?);
            if timing {
                eprintln!("    wmi connect: {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
            }
        }
        let started = std::time::Instant::now();
        let result = run_query(connection, class_name);
        if timing {
            eprintln!("    wmi {class_name}: {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
        }
        result
    }

    fn run_query(connection: &mut Option<WMIConnection>, class_name: &str) -> Result<Vec<WmiObject>, String> {
        let connection = connection.as_ref().expect("connection was just created");
        let select = select_list(class_name);
        let rows: Vec<HashMap<String, Variant>> = match connection.raw_query(format!("SELECT {select} FROM {class_name}")) {
            Ok(rows) => rows,
            // A Windows that lacks one of the named properties rejects the
            // whole list; every property, slow sample and all, is better
            // than none.
            Err(_) if select != "*" => {
                connection.raw_query(format!("SELECT * FROM {class_name}")).map_err(|e| e.to_string())?
            }
            Err(error) => return Err(error.to_string()),
        };
        Ok(rows
            .into_iter()
            .map(|row| WmiObject {
                fields: row.into_iter().map(|(k, v)| (k, convert(v))).collect(),
            })
            .collect())
    }

    /// Every property of Win32_Processor but LoadPercentage. Asking for it
    /// makes WMI sample the processor for a full second before answering,
    /// and nothing here reads it: `SELECT *` cost a second of startup each
    /// time the class was queried.
    const PROCESSOR_PROPERTIES: &str = "AddressWidth, Architecture, AssetTag, Availability, Caption,         Characteristics, ConfigManagerErrorCode, ConfigManagerUserConfig, CpuStatus, CreationClassName,         CurrentClockSpeed, CurrentVoltage, DataWidth, Description, DeviceID, ErrorCleared, ErrorDescription,         ExtClock, Family, InstallDate, L2CacheSize, L2CacheSpeed, L3CacheSize, L3CacheSpeed, LastErrorCode,         Level, Manufacturer, MaxClockSpeed, Name, NumberOfCores, NumberOfEnabledCore,         NumberOfLogicalProcessors, OtherFamilyDescription, PartNumber, PNPDeviceID,         PowerManagementCapabilities, PowerManagementSupported, ProcessorId, ProcessorType, Revision, Role,         SecondLevelAddressTranslationExtensions, SerialNumber, SocketDesignation, Status, StatusInfo,         Stepping, SystemCreationClassName, SystemName, ThreadCount, UniqueId, UpgradeMethod, Version,         VirtualizationFirmwareEnabled, VMMonitorModeExtensions, VoltageCaps";

    fn select_list(class_name: &str) -> &'static str {
        if class_name.eq_ignore_ascii_case("Win32_Processor") { PROCESSOR_PROPERTIES } else { "*" }
    }

    pub fn query(class_name: &str) -> Result<Vec<WmiObject>, String> {
        if !class_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("invalid WMI class name {class_name:?}"));
        }
        let (reply, answer) = mpsc::channel();
        worker()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send((class_name.to_string(), reply))
            .map_err(|_| "the WMI thread has stopped".to_string())?;
        answer.recv().map_err(|_| "the WMI thread has stopped".to_string())?
    }
}

/// A canned WMI source for tests: class name -> objects.
#[derive(Clone, Default)]
pub struct FakeWmi {
    pub classes: BTreeMap<String, Vec<WmiObject>>,
    /// Classes whose query fails, as WMI being unavailable would.
    pub failing: Vec<String>,
}

impl FakeWmi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn class(mut self, name: &str, objects: Vec<WmiObject>) -> Self {
        self.classes.insert(name.to_string(), objects);
        self
    }

    pub fn failing(mut self, name: &str) -> Self {
        self.failing.push(name.to_string());
        self
    }

    /// Install for the calling thread (and clear its static-class cache).
    pub fn install(self) {
        clear_static_cache();
        set_thread_source(Some(Arc::new(self)));
    }
}

impl WmiSource for FakeWmi {
    fn query(&self, class_name: &str) -> Result<Vec<WmiObject>, String> {
        if self.failing.iter().any(|name| name == class_name) {
            return Err(format!("{class_name} unavailable"));
        }
        Ok(self.classes.get(class_name).cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessors_follow_getattr_semantics() {
        let obj = WmiObject::new()
            .with("Name", "Ryzen 9 9950X")
            .with("Capacity", "17179869184")
            .with("Speed", 6000i64)
            .with("Nothing", WmiValue::Null);
        assert_eq!(obj.text("Name").as_deref(), Some("Ryzen 9 9950X"));
        assert_eq!(obj.text("name").as_deref(), Some("Ryzen 9 9950X"));
        assert_eq!(obj.uint("Capacity"), Some(17179869184));
        assert_eq!(obj.int("Speed"), Some(6000));
        assert_eq!(obj.text("Nothing"), None);
        assert_eq!(obj.str_or_empty("Missing"), "");
    }

    #[test]
    fn fake_source_is_per_thread_and_static_cache_holds() {
        FakeWmi::new()
            .class("Win32_Processor", vec![WmiObject::new().with("Name", "A")])
            .install();
        assert_eq!(static_class("Win32_Processor")[0].text("Name").as_deref(), Some("A"));
        FakeWmi::new()
            .class("Win32_Processor", vec![WmiObject::new().with("Name", "B")])
            .install();
        assert_eq!(query_first("Win32_Processor").unwrap().text("Name").as_deref(), Some("B"));
        let elsewhere = std::thread::spawn(|| query("Win32_Processor").is_err())
            .join()
            .unwrap();
        assert!(elsewhere, "another thread must not see this thread's fake");
    }

    #[test]
    fn failures_are_cached_as_empty() {
        FakeWmi::new().failing("Win32_BaseBoard").install();
        assert!(static_class("Win32_BaseBoard").is_empty());
    }
}
