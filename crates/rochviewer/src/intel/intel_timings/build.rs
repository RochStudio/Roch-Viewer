//! Running the passes, in the Python module's order, and handing the result
//! over as [`Row`]s.

use std::sync::{Arc, OnceLock};

use crate::hardware::read::{FieldRead, ReadKind, ReadSpec};
use crate::platform_profiles::Platform;
use crate::row::{Extra, Formula, Row, Side, Value};

use super::dict::{T, V, is_dual_timing};
use super::formulas::Formulas;
use super::{FINALIZED, FinalTable, channel_b_offset, get_trefi_ns_with, get_trfc_ns_with, MCHBAR};

/// A reader of a derived nanosecond row, for one controller base.
pub(crate) type NsReader = Arc<dyn Fn(Option<u64>) -> Option<String> + Send + Sync>;

/// The table under construction and what the passes consult while building
/// it: the Python module's globals, gathered.
pub(crate) struct Build {
    pub platform: Platform,
    /// `CHANNEL_B_OFFSET`: how far a channel-A register's twin sits.
    pub channel_b_offset: u64,
    /// `CHANNEL_B`: `MCHBAR + CHANNEL_B_OFFSET`.
    pub channel_b: u64,
    pub f: Formulas,
    pub rows: Vec<T>,
    /// `CHANNEL_A_LABEL, CHANNEL_B_LABEL`, resolved where the Python resolved
    /// them: after the CAS-to-CAS rows, before the dual-channel pass.
    pub channel_labels: (String, String),
    /// The finished table's field reads, which the derived nanosecond rows
    /// look their intervals up in at render time (the Python read the module
    /// global `TIMINGS` then).
    pub finalized: Arc<OnceLock<FinalTable>>,
}

impl Build {
    pub(crate) fn new(platform: Platform) -> Self {
        let offset = channel_b_offset(platform);
        Build {
            platform,
            channel_b_offset: offset,
            channel_b: MCHBAR + offset,
            f: Formulas::new(),
            rows: Vec::new(),
            channel_labels: ("ChA".into(), "ChB".into()),
            finalized: Arc::new(OnceLock::new()),
        }
    }

    pub(crate) fn trfc_ns_reader(&self) -> NsReader {
        let (platform, finalized) = (self.platform, self.finalized.clone());
        Arc::new(move |base| get_trfc_ns_with(platform, base, finalized.get()?))
    }

    pub(crate) fn trefi_ns_reader(&self) -> NsReader {
        let finalized = self.finalized.clone();
        Arc::new(move |base| get_trefi_ns_with(base, finalized.get()?))
    }

    /// Every pass, in the order the module body ran them.
    pub(crate) fn run(mut self) -> Vec<Row> {
        self.rows = super::table::declared(&self);
        self.install_platform_vref_additional();
        self.normalise_main_rows();
        self.install_custom_general_rows();
        self.install_arrow_lake_main_timings();
        self.install_trdpre_alias();
        self.reorder_power_down_timings();
        self.install_ddr4_skew_live_rows();
        self.reorder_main_secondary_timings();
        self.install_ddr4_odt_rows();
        self.install_arrow_lake_vref_rows();
        self.install_slew_rate_rows();
        self.organize_skew_sections();
        self.install_ddr4_odtl_values();
        self.install_ddr4_refresh_mode_value();
        self.install_system_info_and_timings_tabs();
        self.install_arrow_lake_power_down_rows();
        self.install_trfc_ns_row();
        self.install_trefi_ns_row();
        self.install_dfe_rows();
        self.remove_jedec_tab();
        self.place_misc_additional_after_vref();
        self.install_ccd_timings();
        self.resolve_channel_labels();
        self.install_dual_channel_timings();
        self.point_mode_register_rows_at_channel_b();
        self.install_sensors_tab();
        self.install_refresh_mode_row();
        self.install_refresh_mode_dimming();
        self.install_timings_sections();
        self.group_timings_sections();
        self.install_timings_column_layout();
        self.install_system_info_identity_rows();
        self.install_system_info_order();
        self.install_system_info_sections();
        self.install_misc_tab();
        self.move_mode_register_timings();
        self.move_refresh_policy_rows();
        self.install_misc_columns();
        self.install_misc_section_order();
        self.install_ddr5_timing_labels();
        self.remove_inapplicable_ddr4_timings();
        self.place_skew_columns();
        self.install_skew_without_blank_rows();
        self.promote_ddr5_module_rows();
        self.move_shared_skew_rows_to_phy();
        self.split_misc_sources();
        self.combine_intel_detail_tabs();
        self.install_additional_ddr4_controller_fields();
        self.move_module_refresh_mode_to_timings();
        self.move_channel_registers_to_training();
        self.install_reference_signal_presentation();
        // A few generation-specific rows are installed after the first Timings layout
        // pass. Reapply the stable section ordering once the table is complete so late
        // rows such as tRFC2 land beside their family instead of at the section tail.
        self.group_timings_sections();
        self.install_ddr4_mode_register_rows();
        self.organize_ddr4_training_sections();
        self.organize_ddr4_imc_sections();
        self.present_ddr5_rows();

        let table = finalized_fields(&self.rows);
        let _ = self.finalized.set(table.clone());
        FINALIZED.insert(self.platform, Arc::new(table));
        self.rows.iter().map(convert).collect()
    }
}

/// `_read_finalized_timing`'s view of the table: each row that names an
/// address and a bit field, in table order.
fn finalized_fields(rows: &[T]) -> FinalTable {
    rows.iter()
        .filter_map(|row| {
            let name = row.name()?.to_string();
            let address = row.int("address")?;
            let (bit_start, bit_length) = row.params("parameters")?;
            Some((name, FieldRead::new(address, bit_start, bit_length)))
        })
        .collect()
}

/// A row value as the display resolves it.
fn value_of(value: &V) -> Option<Value> {
    match value {
        V::Str(text) => Some(Value::Text(text.clone())),
        V::Int(number) => Some(Value::Text(number.to_string())),
        V::Bool(flag) => Some(Value::Text(if *flag { "True".into() } else { "False".into() })),
        V::Text(read) => Some(Value::Lazy(read.clone())),
        _ => None,
    }
}

/// `direct_read_type`: the first of the keys naming a direct read width.
fn read_kind(row: &T, keys: &[&str]) -> ReadKind {
    for key in keys {
        match row.str(key) {
            Some("wide") => return ReadKind::Wide,
            Some("standard") => return ReadKind::Standard,
            _ => {}
        }
    }
    ReadKind::Standard
}

/// What a row reads directly: its dynamic search, or its register field.
fn read_of(row: &T, suffix: &str) -> Option<ReadSpec> {
    let key = |base: &str| -> String { if suffix.is_empty() { base.to_string() } else { format!("{base}_{suffix}") } };
    if row.is(&key("read_type"), "dynamic")
        && let Some(params) = row.dynamic(&key("dynamic_params")) {
            return Some(ReadSpec::Dynamic(params));
        }
    let address = row.int(&key("address"))?;
    let (bit_start, bit_length) = row.params(&key("parameters"))?;
    let read_type_key = key("read_type");
    let kind = read_kind(row, &[read_type_key.as_str(), "read_type"]);
    Some(ReadSpec::Field(FieldRead { address, bit_start, bit_length, kind }))
}

fn side_of(row: &T, suffix: &str) -> Option<Side> {
    let name = row.str(&format!("name_{suffix}")).map(str::to_string);
    let value = match row.get(&format!("value_{suffix}")) {
        // A side whose value key is present resolves it, None and all: the
        // Python's read_side checked for the key, not for a value.
        Some(V::None) => Some(Value::lazy(|| None)),
        Some(value) => value_of(value),
        None => None,
    };
    let read = read_of(row, suffix);
    if name.is_none() && value.is_none() && read.is_none() {
        return None;
    }
    Some(Side { name, value, read })
}

/// Keys the conversion maps onto `Row` fields; the rest go to `extra`.
const MAPPED_KEYS: &[&str] = &[
    "name",
    "display_name",
    "Category",
    "Tab",
    "Column",
    "value",
    "address",
    "parameters",
    "read_type",
    "dynamic_params",
    "Formula",
    "name_a",
    "name_b",
    "value_a",
    "value_b",
    "address_a",
    "address_b",
    "parameters_a",
    "parameters_b",
    "read_type_a",
    "read_type_b",
    "dynamic_params_a",
    "dynamic_params_b",
    "default_value",
    "live",
    "dim",
    "lone_channel_alone",
];

/// One finished row dict as a [`Row`].
pub(crate) fn convert(row: &T) -> Row {
    let mut out = Row::new(
        row.name().unwrap_or_default(),
        row.category().unwrap_or_default(),
        row.tab().unwrap_or_default(),
        row.column().unwrap_or_default(),
    );
    out.display_name = row.str("display_name").map(str::to_string);
    out.value = row.get("value").and_then(value_of);
    out.read = read_of(row, "");
    if let Some(V::Map(map)) = row.get("Formula") {
        out.formula = Some(Formula::Map(map.clone()));
    }
    if is_dual_timing(row) {
        out.a = side_of(row, "a");
        out.b = side_of(row, "b");
    }
    out.default_value = row.str("default_value").map(str::to_string);
    out.live = matches!(row.get("live"), Some(V::Bool(true)));
    out.lone_channel_alone = matches!(row.get("lone_channel_alone"), Some(V::Bool(true)));
    if let Some(V::Pred(dim)) = row.get("dim") {
        out.dim = Some(dim.clone());
    }
    for (key, value) in &row.m {
        if MAPPED_KEYS.contains(key) {
            continue;
        }
        let extra = match value {
            V::Bool(flag) => Extra::Bool(*flag),
            V::Int(number) => Extra::Int(*number as i64),
            V::Str(text) => Extra::Str(text.clone()),
            V::Pred(pred) => Extra::Pred(pred.clone()),
            V::Text(read) => Extra::Text(read.clone()),
            V::Base(read) => Extra::BaseReader(read.clone()),
            V::None | V::Params(_) | V::Dyn(_) | V::Map(_) => continue,
        };
        out.extra.insert(key.to_string(), extra);
    }
    debug_assert_eq!(
        out.is_dual(),
        is_dual_timing(row),
        "row {:?} converts to a different dual shape than the Python rule gives",
        row.name()
    );
    out
}
