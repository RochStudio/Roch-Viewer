//! One displayed reading: the Rust form of the Python profiles' row dicts.
//!
//! The Python built each platform's table as a list of dicts with keys such
//! as `name`, `Tab`, `Category`, `Column`, `value` (text or a callable),
//! `address` + `parameters` (a register field), `dynamic_params` (a search of
//! the mode-register table), `Formula` (a dict or callable applied to a raw
//! read), and paired `_a` / `_b` variants for rows that show two channels.
//! [`Row`] carries the same information with a type for each of those
//! shapes; keys used by only a handful of rows live in [`Row::extra`].
//!
//! Key mapping, for anyone porting a table:
//!
//! | Python key                        | Rust                               |
//! |-----------------------------------|------------------------------------|
//! | `name`                            | `name`                             |
//! | `display_name`                    | `display_name`                     |
//! | `Category` / `Tab` / `Column`     | `category` / `tab` / `column`      |
//! | `value` (str/number)              | `value: Some(Value::Text)`         |
//! | `value` (callable)                | `value: Some(Value::Lazy)`         |
//! | `address` + `parameters` + `read_type` | `read: Some(ReadSpec::Field)` |
//! | `dynamic_params` + `read_type="dynamic"` | `read: Some(ReadSpec::Dynamic)` |
//! | `Formula`                         | `formula`                          |
//! | `name_a`, `value_a`, `address_a`... | `a: Some(Side { .. })`           |
//! | `default_value`                   | `default_value`                    |
//! | `live`, `refresh`, `dim`          | `live`, `refresh`, `dim`           |
//! | anything else                     | `extra["key"]`                     |

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

pub use crate::hardware::read::{DynamicParams, FieldRead, ReadKind, ReadSpec};

/// A deferred reading producing display text, or `None` when unavailable.
pub type TextFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;
/// A deferred yes/no question (the Python `dim` callables).
pub type PredFn = Arc<dyn Fn() -> bool + Send + Sync>;
/// A side effect with no result (the Python `refresh` callables).
pub type ActionFn = Arc<dyn Fn() + Send + Sync>;
/// A formula applied to a raw register value.
pub type FormulaFn = Arc<dyn Fn(u64) -> String + Send + Sync>;
/// A reader parameterised by a controller base address.
pub type BaseReaderFn = Arc<dyn Fn(u64) -> Option<String> + Send + Sync>;

/// What a raw read becomes on screen.
#[derive(Clone)]
pub enum Formula {
    /// A lookup table; a value missing from it shows as unavailable.
    Map(Arc<BTreeMap<u64, String>>),
    /// A computation.
    Func(FormulaFn),
}

impl Formula {
    pub fn map<S: AsRef<str>>(pairs: &[(u64, S)]) -> Self {
        Formula::Map(Arc::new(
            pairs.iter().map(|(k, v)| (*k, v.as_ref().to_string())).collect(),
        ))
    }

    pub fn func(f: impl Fn(u64) -> String + Send + Sync + 'static) -> Self {
        Formula::Func(Arc::new(f))
    }
}

impl fmt::Debug for Formula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Formula::Map(map) => f.debug_tuple("Map").field(map).finish(),
            Formula::Func(_) => f.write_str("Func(..)"),
        }
    }
}

/// A row's own value: fixed text or a deferred reading.
#[derive(Clone)]
pub enum Value {
    Text(String),
    Lazy(TextFn),
}

impl Value {
    pub fn text(text: impl Into<String>) -> Self {
        Value::Text(text.into())
    }

    pub fn lazy(f: impl Fn() -> Option<String> + Send + Sync + 'static) -> Self {
        Value::Lazy(Arc::new(f))
    }

    /// Evaluate: Python's `resolve_display_value`. Unavailable readings show
    /// as `unavailable`.
    pub fn resolve(&self, unavailable: &str) -> String {
        match self {
            Value::Text(text) => text.clone(),
            Value::Lazy(f) => {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f())) {
                    Ok(Some(text)) => text,
                    _ => unavailable.to_string(),
                }
            }
        }
    }

    pub fn is_lazy(&self) -> bool {
        matches!(self, Value::Lazy(_))
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Text(text) => f.debug_tuple("Text").field(text).finish(),
            Value::Lazy(_) => f.write_str("Lazy(..)"),
        }
    }
}

impl From<&str> for Value {
    fn from(text: &str) -> Self {
        Value::Text(text.to_string())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Self {
        Value::Text(text)
    }
}

/// One channel's half of a dual row.
#[derive(Clone, Debug, Default)]
pub struct Side {
    /// `name_a` / `name_b`: the column heading for this channel.
    pub name: Option<String>,
    /// `value_a` / `value_b`.
    pub value: Option<Value>,
    /// `address_a` + `parameters_a` (+ `read_type_a`), or `dynamic_params_a`.
    pub read: Option<ReadSpec>,
}

impl Side {
    pub fn read(spec: ReadSpec) -> Self {
        Side { read: Some(spec), ..Side::default() }
    }

    pub fn value(value: Value) -> Self {
        Side { value: Some(value), ..Side::default() }
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Keys only a few rows carry.
#[derive(Clone)]
pub enum Extra {
    Bool(bool),
    Int(i64),
    Str(String),
    Pred(PredFn),
    Text(TextFn),
    Action(ActionFn),
    BaseReader(BaseReaderFn),
}

impl fmt::Debug for Extra {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Extra::Bool(b) => write!(f, "Bool({b})"),
            Extra::Int(i) => write!(f, "Int({i})"),
            Extra::Str(s) => write!(f, "Str({s:?})"),
            Extra::Pred(_) => f.write_str("Pred(..)"),
            Extra::Text(_) => f.write_str("Text(..)"),
            Extra::Action(_) => f.write_str("Action(..)"),
            Extra::BaseReader(_) => f.write_str("BaseReader(..)"),
        }
    }
}

#[derive(Clone, Default)]
pub struct Row {
    pub name: String,
    pub display_name: Option<String>,
    pub category: String,
    pub tab: String,
    pub column: String,
    pub value: Option<Value>,
    pub read: Option<ReadSpec>,
    pub formula: Option<Formula>,
    pub a: Option<Side>,
    pub b: Option<Side>,
    pub default_value: Option<String>,
    /// Re-read on the live refresh tick (voltages, clocks, power).
    pub live: bool,
    /// Called before a tab is re-read (clears a cache the row reads from).
    pub refresh: Option<ActionFn>,
    /// When true, the value is shown dimmed: readable but not in effect.
    pub dim: Option<PredFn>,
    /// A dual row whose single present channel stands alone ("Disabled",
    /// not "Disabled/-").
    pub lone_channel_alone: bool,
    pub extra: BTreeMap<String, Extra>,
}

impl fmt::Debug for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Row")
            .field("name", &self.name)
            .field("tab", &self.tab)
            .field("category", &self.category)
            .field("column", &self.column)
            .field("value", &self.value)
            .field("read", &self.read)
            .field("a", &self.a)
            .field("b", &self.b)
            .finish_non_exhaustive()
    }
}

impl Row {
    pub fn new(
        name: impl Into<String>,
        category: impl Into<String>,
        tab: impl Into<String>,
        column: impl Into<String>,
    ) -> Self {
        Row {
            name: name.into(),
            category: category.into(),
            tab: tab.into(),
            column: column.into(),
            ..Row::default()
        }
    }

    pub fn with_value(mut self, value: impl Into<Value>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn with_lazy(mut self, f: impl Fn() -> Option<String> + Send + Sync + 'static) -> Self {
        self.value = Some(Value::lazy(f));
        self
    }

    pub fn with_read(mut self, spec: ReadSpec) -> Self {
        self.read = Some(spec);
        self
    }

    pub fn with_field(self, field: FieldRead) -> Self {
        self.with_read(ReadSpec::Field(field))
    }

    pub fn with_formula(mut self, formula: Formula) -> Self {
        self.formula = Some(formula);
        self
    }

    pub fn with_sides(mut self, a: Side, b: Side) -> Self {
        self.a = Some(a);
        self.b = Some(b);
        self
    }

    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    pub fn live(mut self) -> Self {
        self.live = true;
        self
    }

    pub fn with_extra(mut self, key: &str, value: Extra) -> Self {
        self.extra.insert(key.to_string(), value);
        self
    }

    /// The label shown for this row.
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }

    /// Python's `is_dual_timing`: does the row display two channels?
    pub fn is_dual(&self) -> bool {
        let field = |side: &Option<Side>| {
            matches!(side.as_ref().and_then(|s| s.read), Some(ReadSpec::Field(_)))
        };
        let dynamic = |side: &Option<Side>| {
            matches!(side.as_ref().and_then(|s| s.read), Some(ReadSpec::Dynamic(_)))
        };
        let has_value = |side: &Option<Side>| side.as_ref().is_some_and(|s| s.value.is_some());
        (field(&self.a) && field(&self.b))
            || (dynamic(&self.a) && dynamic(&self.b))
            || has_value(&self.a)
            || has_value(&self.b)
    }

    pub fn side(&self, channel: Channel) -> Option<&Side> {
        match channel {
            Channel::A => self.a.as_ref(),
            Channel::B => self.b.as_ref(),
        }
    }

    pub fn extra_bool(&self, key: &str) -> bool {
        match self.extra.get(key) {
            Some(Extra::Bool(b)) => *b,
            Some(Extra::Pred(p)) => p(),
            Some(Extra::Int(i)) => *i != 0,
            Some(Extra::Str(s)) => !s.is_empty(),
            Some(_) => true,
            None => false,
        }
    }

    pub fn extra_str(&self, key: &str) -> Option<&str> {
        match self.extra.get(key) {
            Some(Extra::Str(s)) => Some(s),
            _ => None,
        }
    }

    pub fn extra_int(&self, key: &str) -> Option<i64> {
        match self.extra.get(key) {
            Some(Extra::Int(i)) => Some(*i),
            _ => None,
        }
    }

    /// Whether the row should be shown dimmed right now.
    pub fn is_dimmed(&self) -> bool {
        self.dim.as_ref().is_some_and(|dim| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dim())).unwrap_or(false)
        })
    }
}

/// A memory channel of a dual row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    A,
    B,
}

impl Channel {
    pub fn suffix(self) -> &'static str {
        match self {
            Channel::A => "a",
            Channel::B => "b",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dual_detection_follows_the_python_rule() {
        let field = ReadSpec::Field(FieldRead::new(0x10, 0, 4));
        let row = Row::new("tCL", "Primary", "Timings", "Left")
            .with_sides(Side::read(field), Side::read(field));
        assert!(row.is_dual());

        let single_side = Row {
            a: Some(Side::read(field)),
            ..Row::new("x", "c", "t", "Left")
        };
        assert!(!single_side.is_dual());

        let value_b = Row {
            b: Some(Side::value("40 ohm".into())),
            ..Row::new("x", "c", "t", "Left")
        };
        assert!(value_b.is_dual());
    }

    #[test]
    fn lazy_values_resolve_and_fail_soft() {
        assert_eq!(Value::lazy(|| Some("1.25 V".into())).resolve("—"), "1.25 V");
        assert_eq!(Value::lazy(|| None).resolve("—"), "—");
        assert_eq!(Value::lazy(|| panic!("boom")).resolve("—"), "—");
    }

    #[test]
    fn label_prefers_display_name() {
        let row = Row::new("tRFC1", "Secondary", "Timings", "Left").with_display_name("tRFC");
        assert_eq!(row.label(), "tRFC");
    }
}
