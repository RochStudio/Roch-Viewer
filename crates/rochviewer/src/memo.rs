//! Process-wide caches that stay isolated per test.
//!
//! The Python used `functools.lru_cache` and module-level lists for readings
//! that cannot change while the machine runs (identity, the DDR generation,
//! a detected sensor profile), and its tests cleared them between cases.
//! Rust tests run on parallel threads, so a plain global cache would leak one
//! test's stubbed hardware into another. Under `cfg(test)` every cache here
//! is keyed by the calling thread, which gives each test its own empty cache
//! without anyone remembering to clear it; in a real build it is one shared
//! value. The `testing` feature turns the per-thread behaviour on for
//! crates that test against this one (the window's tests).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;

#[cfg(any(test, feature = "testing"))]
type Scope = std::thread::ThreadId;
#[cfg(not(any(test, feature = "testing")))]
type Scope = ();

#[cfg(any(test, feature = "testing"))]
fn scope() -> Scope {
    std::thread::current().id()
}
#[cfg(not(any(test, feature = "testing")))]
fn scope() -> Scope {}

/// A value computed once (per test thread under `cfg(test)`).
pub struct Memo<T> {
    slots: Mutex<Option<HashMap<Scope, T>>>,
}

impl<T: Clone> Memo<T> {
    pub const fn new() -> Self {
        Self { slots: Mutex::new(None) }
    }

    /// The cached value, computing it with `init` the first time.
    ///
    /// `init` runs without the lock held, so it may itself use other memos.
    /// Two threads racing the first call may both compute; the first to
    /// finish wins, which is harmless for the read-only values cached here.
    pub fn get_or_init(&self, init: impl FnOnce() -> T) -> T {
        if let Some(value) = self.get() {
            return value;
        }
        let value = init();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots
            .get_or_insert_with(HashMap::new)
            .entry(scope())
            .or_insert(value)
            .clone()
    }

    pub fn get(&self) -> Option<T> {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.as_ref().and_then(|map| map.get(&scope()).cloned())
    }

    pub fn set(&self, value: T) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots.get_or_insert_with(HashMap::new).insert(scope(), value);
    }

    pub fn clear(&self) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(map) = slots.as_mut() {
            map.remove(&scope());
        }
    }
}

impl<T: Clone> Default for Memo<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A keyed cache: `lru_cache(maxsize=None)` on a function with arguments.
pub struct MemoMap<K, V> {
    slots: Mutex<Option<HashMap<(Scope, K), V>>>,
}

impl<K: Eq + Hash + Clone, V: Clone> MemoMap<K, V> {
    pub const fn new() -> Self {
        Self { slots: Mutex::new(None) }
    }

    pub fn get_or_init(&self, key: K, init: impl FnOnce() -> V) -> V {
        if let Some(value) = self.get(&key) {
            return value;
        }
        let value = init();
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots
            .get_or_insert_with(HashMap::new)
            .entry((scope(), key))
            .or_insert(value)
            .clone()
    }

    pub fn get(&self, key: &K) -> Option<V> {
        let slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots
            .as_ref()
            .and_then(|map| map.get(&(scope(), key.clone())).cloned())
    }

    pub fn insert(&self, key: K, value: V) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        slots
            .get_or_insert_with(HashMap::new)
            .insert((scope(), key), value);
    }

    pub fn clear(&self) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(map) = slots.as_mut() {
            #[cfg(any(test, feature = "testing"))]
            {
                let mine = scope();
                map.retain(|(owner, _), _| *owner != mine);
            }
            #[cfg(not(any(test, feature = "testing")))]
            map.clear();
        }
    }
}

impl<K: Eq + Hash + Clone, V: Clone> Default for MemoMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: Memo<u32> = Memo::new();

    #[test]
    fn computes_once() {
        let mut calls = 0;
        assert_eq!(COUNTER.get_or_init(|| { calls += 1; 7 }), 7);
        assert_eq!(COUNTER.get_or_init(|| { calls += 1; 9 }), 7);
        assert_eq!(calls, 1);
        COUNTER.clear();
        assert_eq!(COUNTER.get_or_init(|| 9), 9);
    }

    #[test]
    fn each_test_thread_starts_empty() {
        assert_eq!(COUNTER.get(), None);
        let other = std::thread::spawn(|| COUNTER.get_or_init(|| 1)).join().unwrap();
        assert_eq!(other, 1);
        assert_eq!(COUNTER.get(), None);
    }

    #[test]
    fn keyed() {
        static SQUARES: MemoMap<u32, u32> = MemoMap::new();
        assert_eq!(SQUARES.get_or_init(3, || 9), 9);
        assert_eq!(SQUARES.get_or_init(3, || 0), 9);
        assert_eq!(SQUARES.get(&4), None);
    }
}
