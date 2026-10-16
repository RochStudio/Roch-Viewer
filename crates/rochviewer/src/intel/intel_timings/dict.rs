//! The row as the Python built it: a dict, mutated by pass after pass.
//!
//! The Intel table is not declared in its final shape. It is a literal of
//! row dicts that some forty passes then rename, re-file, promote to two
//! channels, strip of keys and re-key, each pass asking whether a key is
//! *present* as often as what it holds -- `"parameters_a" in timing` decides
//! whether a row is dual, `timing.pop("value", None)` is how a pass says a row
//! is now a register read. Porting those passes onto [`crate::row::Row`]
//! directly would mean re-deriving, pass by pass, which presence test each
//! field stands for. So the builder works on this: an ordered set of keys
//! with the handful of value shapes the Python stored under them, and the
//! finished table is converted to `Row` once, at the end (see `convert`).
//!
//! Identity is explicit. The Python compared rows with `in`, `.index()` and
//! `.remove()`, which match the first row that is *equal*; no two rows in the
//! table are ever equal, so that is identity in practice, and `id` makes it
//! identity by construction. `dict(timing)` -- a copy -- gets a new one.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::hardware::read::DynamicParams;
use crate::row::{BaseReaderFn, PredFn, TextFn};

/// What a key held.
#[derive(Clone)]
pub(crate) enum V {
    /// Python `None`, stored explicitly (`"value": None`).
    None,
    Bool(bool),
    /// An address or an offset.
    Int(u64),
    Str(String),
    /// `"parameters"`: `{}` or `{"bit_start": s, "bit_length": l}`.
    Params(Option<(u32, i32)>),
    /// `"dynamic_params"`.
    Dyn(DynamicParams),
    /// A `Formula` dict.
    Map(Arc<BTreeMap<u64, String>>),
    /// A callable value: `value`, `value_a`, `value_b`.
    Text(TextFn),
    /// The `dim` callable.
    Pred(PredFn),
    /// `base_reader`: a reader taking the controller base.
    Base(BaseReaderFn),
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// One row dict.
pub(crate) struct T {
    pub id: u64,
    pub m: BTreeMap<&'static str, V>,
}

impl T {
    pub fn new() -> Self {
        T { id: next_id(), m: BTreeMap::new() }
    }

    /// `dict(timing)`: the same keys, a different row.
    pub fn copy(&self) -> Self {
        T { id: next_id(), m: self.m.clone() }
    }

    pub fn with(mut self, key: &'static str, value: V) -> Self {
        self.m.insert(key, value);
        self
    }

    pub fn s(mut self, key: &'static str, value: &str) -> Self {
        self.m.insert(key, V::Str(value.to_string()));
        self
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.m.get(key)
    }

    pub fn has(&self, key: &str) -> bool {
        self.m.contains_key(key)
    }

    /// `timing.get(key)` when it holds text.
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.m.get(key) {
            Some(V::Str(text)) => Some(text),
            _ => None,
        }
    }

    /// `timing.get(key) == value`.
    pub fn is(&self, key: &str, value: &str) -> bool {
        self.str(key) == Some(value)
    }

    pub fn name(&self) -> Option<&str> {
        self.str("name")
    }

    pub fn tab(&self) -> Option<&str> {
        self.str("Tab")
    }

    pub fn category(&self) -> Option<&str> {
        self.str("Category")
    }

    pub fn column(&self) -> Option<&str> {
        self.str("Column")
    }

    /// `timing.get(key)` when it holds an address.
    pub fn int(&self, key: &str) -> Option<u64> {
        match self.m.get(key) {
            Some(V::Int(value)) => Some(*value),
            _ => None,
        }
    }

    /// `timing.get(key)` when it is a `{bit_start, bit_length}` dict with
    /// both keys.
    pub fn params(&self, key: &str) -> Option<(u32, i32)> {
        match self.m.get(key) {
            Some(V::Params(Some(bits))) => Some(*bits),
            _ => None,
        }
    }

    pub fn dynamic(&self, key: &str) -> Option<DynamicParams> {
        match self.m.get(key) {
            Some(V::Dyn(params)) => Some(*params),
            _ => None,
        }
    }

    pub fn text_fn(&self, key: &str) -> Option<TextFn> {
        match self.m.get(key) {
            Some(V::Text(f)) => Some(f.clone()),
            _ => None,
        }
    }

    pub fn base_fn(&self, key: &str) -> Option<BaseReaderFn> {
        match self.m.get(key) {
            Some(V::Base(f)) => Some(f.clone()),
            _ => None,
        }
    }

    pub fn set(&mut self, key: &'static str, value: V) {
        self.m.insert(key, value);
    }

    pub fn set_str(&mut self, key: &'static str, value: &str) {
        self.m.insert(key, V::Str(value.to_string()));
    }

    /// `timing.setdefault(key, value)` for text.
    pub fn set_default_str(&mut self, key: &'static str, value: &str) {
        self.m.entry(key).or_insert_with(|| V::Str(value.to_string()));
    }

    pub fn pop(&mut self, key: &str) -> Option<V> {
        self.m.remove(key)
    }

    pub fn pop_all(&mut self, keys: &[&str]) {
        for key in keys {
            self.m.remove(*key);
        }
    }
}

/// The text of a value, if it is text.
pub(crate) fn text(text: impl Into<String>) -> V {
    V::Str(text.into())
}

/// A callable value.
pub(crate) fn lazy(f: impl Fn() -> Option<String> + Send + Sync + 'static) -> V {
    V::Text(Arc::new(f))
}

/// A `{bit_start, bit_length}` parameters dict.
pub(crate) fn bits(bit_start: u32, bit_length: i32) -> V {
    V::Params(Some((bit_start, bit_length)))
}

/// An empty parameters dict, `{}`.
pub(crate) fn no_bits() -> V {
    V::Params(None)
}

/// A result the Python would have stored as `None` or as a string.
pub(crate) fn opt(value: Option<String>) -> V {
    match value {
        Some(text) => V::Str(text),
        None => V::None,
    }
}

/// Python's `is_dual_timing`, asked of a row dict.
pub(crate) fn is_dual_timing(timing: &T) -> bool {
    (timing.has("parameters_a") && timing.has("parameters_b"))
        || (timing.has("dynamic_params_a") && timing.has("dynamic_params_b"))
        || timing.has("value_a")
        || timing.has("value_b")
}

/// The keys a promotion adds and a demotion strips.
pub(crate) const DUAL_KEYS: &[&str] = &[
    "address_a",
    "address_b",
    "parameters_a",
    "parameters_b",
    "read_type_a",
    "read_type_b",
    "value_a",
    "value_b",
    "name_a",
    "name_b",
    "parameter_name",
    "dynamic_params_a",
    "dynamic_params_b",
];

/// Index of the row with this identity.
pub(crate) fn index_of(rows: &[T], id: u64) -> Option<usize> {
    rows.iter().position(|row| row.id == id)
}

/// `TIMINGS.remove(row)`: take the row with this identity out and return it.
pub(crate) fn remove_id(rows: &mut Vec<T>, id: u64) -> Option<T> {
    index_of(rows, id).map(|index| rows.remove(index))
}

/// Split `rows` into those `pick` selects and the rest, both in order.
pub(crate) fn partition(rows: Vec<T>, pick: impl Fn(&T) -> bool) -> (Vec<T>, Vec<T>) {
    let mut picked = Vec::new();
    let mut rest = Vec::new();
    for row in rows {
        if pick(&row) {
            picked.push(row);
        } else {
            rest.push(row);
        }
    }
    (picked, rest)
}

/// Stable-sort the rows at `positions` among themselves by `key`, leaving
/// every other row where it is: the Python's
/// `rows = [TIMINGS[i] for i in positions]; rows.sort(key=...);
/// for i, row in zip(positions, rows): TIMINGS[i] = row`.
pub(crate) fn sort_in_place<K: Ord>(rows: &mut Vec<T>, positions: &[usize], key: impl Fn(&T) -> K) {
    if positions.is_empty() {
        return;
    }
    let mut taken: Vec<Option<T>> = rows.drain(..).map(Some).collect();
    let mut picked: Vec<T> = positions.iter().map(|&i| taken[i].take().expect("position taken once")).collect();
    picked.sort_by_key(|row| key(row));
    for (&position, row) in positions.iter().zip(picked) {
        taken[position] = Some(row);
    }
    *rows = taken.into_iter().map(|row| row.expect("every slot refilled")).collect();
}
