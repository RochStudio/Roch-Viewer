# Rust conventions

Roch Viewer was rewritten from Python to Rust for 1.1.0. This page is the
contract the modules follow, so modules written separately fit together
without edits to each other. Module names follow the Python ones they
replaced, so the tables below double as a map from the Python source (tag
`v1.0.9` and earlier) to this one.

## Layout

| Python                         | Rust                                             |
| ------------------------------ | ------------------------------------------------ |
| `rochviewer/<pkg>/<mod>.py`    | `crates/rochviewer/src/<pkg>/<mod>.rs`           |
| `rochviewer/<mod>.py`          | `crates/rochviewer/src/<mod>.rs`                 |
| `rochviewer/ui/*.py`           | `crates/roch-viewer-app/src/ui/*.rs` (egui)      |
| `tests/test_<mod>.py`          | `#[cfg(test)] mod tests` in the module it tests  |
| `run_viewer.py`                | `crates/roch-viewer-app/src/main.rs` (`RochViewer.exe`) |

Module and function names stay the same, in snake_case, so a reader can
hold the Python and the Rust side by side. Public constants stay
`SCREAMING_CASE`. Private helpers named `_foo` in Python become private
`fn foo`.

## The foundation (already ported, do not change signatures)

* `hardware::hw()` returns the `Hardware` backend: `read_phys`,
  `get_phys_long`, `set_phys_long`, `inb`/`outb`, `inl`/`outl`,
  `is_driver_open`. Every method returns `HwResult<T>`. The Python
  `InpOutPortIO`/`InpOutByteIO`/`inpout.Inp32`/`DlPort*` all map onto these.
* `hardware::read`: `read_physical_memory`, `read_dword`,
  `read_physical_memory_int`, `extract_value`, `read_field`, `read_timing`,
  `FieldRead`, `DynamicParams`, `ReadSpec`, `ReadKind`.
* `hardware::lowlevel_io`: `NamedMutex::acquire(name, ms)` / `NamedMutex::pci()`
  (RAII guard), CF8/CFC/SMN/RSMU constants, `config_address`,
  `config_address_bdf`.
* `hardware::pci_mcfg`: `parse_mcfg`, `select_allocation`,
  `default_allocation`, `ecam_address`, `probe_mcfg_vendor`,
  `get_mcfg_table`, `read_physical_dword`, `pci_config_dword`.
* `hardware::driver_path`: `find_driver`, `missing_message`.
* `wmi_util`: `query(class)`, `query_first(class)`, `static_class(class)`
  (cached for the process, failure cached as empty), `WmiObject` with
  `text`/`str_or_empty`/`int`/`uint`/`float` accessors that mirror
  `getattr(obj, field, default)`.
* `row`: `Row`, `Side`, `Value`, `Formula`, `Extra`, `Channel`. The table in
  `row.rs`'s module doc maps every Python row key.
* `display_values`: `read_value`, `read_side`, `apply_formula`,
  `module_pair_text`, `summary_compact_ohm`, `select_tab_names`, `EM_DASH`,
  `NOT_AVAILABLE`.
* `platform_profiles`: `Platform`, `classify_platform`,
  `is_granite_ridge_cpu`, `is_raphael_cpu`, `active_platform`,
  `set_active_platform`.
* `memory::ddr_generation`: `DdrGeneration`, `detect_ddr_generation`.
* `sensors::voltage_rails`: the full module.
* `memo::{Memo, MemoMap}` replace `functools.lru_cache` and the module-level
  `_CACHE = []` lists. Under `cfg(test)` they are per test thread, so tests
  never see each other's cached values.
* `pyfmt`: `float_str` (Python `str(float)`), `round_half_even` and
  `round_int` (Python `round`), `fixed` (`"%.Nf"`).

## Translating Python idioms

* **Exceptions.** A function that raised and whose callers caught it returns
  `Result<T, String>` (or `HwResult`); one that returned `None` on failure
  returns `Option<T>`. Keep the Python's error *messages* verbatim where a
  test or the UI shows them.
* **`print(...)` warnings** become `eprintln!` with the same text.
* **Dicts of readings** (`{"vddcr_vdd": 1.25, ...}`) become a struct when
  the keys are fixed, or `BTreeMap<String, f64>` (or `BTreeMap<&'static str,
  _>`) when callers look keys up by name. Prefer whatever keeps the callers
  simplest.
* **Dataclasses** become structs with `#[derive(Clone, Debug, PartialEq)]`.
* **Injected dependencies.** Where the Python took an `io=`, `reader=`,
  `read_dword=`, `wmi_factory=` or similar argument for its tests, prefer
  the global seams (`hardware::set_thread_hardware`, `FakeWmi::install`)
  and drop the parameter. Keep a parameter only when the Python used it for
  real behaviour, not just testing.
* **Text formatting.** Display strings must match the Python byte for byte.
  `"%.3f V" % v` is `format!("{v:.3} V")`; `str(1.0)` is
  `pyfmt::float_str(1.0)` (Rust's `{}` would print `1`); `round(x, 2)` is
  `pyfmt::round_half_even(x, 2)`.
* **Integers.** Register values are `u32`/`u64`; signed math that the Python
  did on unbounded ints should use `i64` and be checked for overflow where it
  matters.
* **Threads and locks.** `threading.Lock()` becomes `std::sync::Mutex<()>`
  in a `static`.

## Comments are part of the port

This project records *how* a register was identified next to its
definition (see CONTRIBUTING.md). Every comment that explains a register,
an offset, a bound, a validation, or why the code is shaped the way it is
must be carried over, reworded only as far as Rust syntax needs. Docstrings
become `///` or `//!` doc comments. Do not summarise them away.

## Tests

Every test in the Python `tests/` file for a module is ported into that
module's `#[cfg(test)] mod tests`, one Rust test per Python test (or per
`subTest` case group), with the same name minus the `test_` prefix. Tests
must not depend on the machine they run on:

* hardware: `crate::testing::FakeHardware` (`with_dword`, `with_port`,
  `with_port_reader`, `install()`, then `.log()` / `.phys_reads()` to assert
  on what was touched). A test may also implement `Hardware` itself.
* WMI: `crate::wmi_util::FakeWmi::new().class("Win32_X", vec![...]).install()`.
* Under `cfg(test)` with nothing installed, every hardware access fails and
  every WMI query errors, so a forgotten stub fails loudly instead of
  reading the real machine.
* Python tests that need live hardware (`*_live`, ones that `skipTest`
  without the driver) are ported with `#[ignore = "needs hardware"]`.

Run them with `cargo test -p rochviewer`. A module is done when it compiles
without warnings, its tests pass, and nothing in it is a stub.
