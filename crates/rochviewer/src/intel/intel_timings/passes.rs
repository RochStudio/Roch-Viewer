//! The passes that shape the declared table, in the order the Python module
//! ran them at import: platform re-points, the Main/System Info split, the
//! Training organisation, the derived rows, and the second channel.

use std::sync::Arc;

use crate::memory::ddr_generation::{DdrGeneration, detect_ddr_generation};
use crate::row::BaseReaderFn;

use super::build::Build;
use super::dict::{
    T, V, bits, index_of, is_dual_timing, lazy, no_bits, opt, partition, remove_id, sort_in_place, text,
};
use super::identity::{
    dimm_field, get_bios_version, get_board_manufacturer, get_board_model, get_cpu_cores_threads, get_cpu_name,
    get_microcode, get_ram_manufacturer, get_total_physical_memory, detect_dual_channel_memory,
};
use super::registers::{
    self, DFE_TAP_COUNT, decode_ddr4_dq_vref, get_bclk_rd, get_ccd_timing, get_cmd_stretch, get_command_rate,
    get_core_clock, get_core_ratio, get_ddr4_refresh_mode_value, get_ddr4_ron, get_ddr4_rtt_nom, get_ddr4_rtt_park,
    get_ddr4_rtt_wr, get_dfe_bias, get_dfe_enable, get_dllk_timing, get_dram_frequency, get_dram_ratio_value,
    get_ecc_error_correction_value, get_gear_mode_value, get_mclk, get_memory_scrambler_value,
    get_power_down_mode_value, get_psf0_pll, get_qclk_ratio, get_ring_freq, get_row_hammer_value, get_sa,
    get_self_refresh_value, get_speed, get_tmod_value, get_trc_value, get_twr_value, get_twtr_l, get_twtr_s, get_tx,
    get_uclk, get_uncore_ratio, is_arrow_lake_platform, slew_rate_category,
};
use super::table::dp;
use super::{MCHBAR, MCHBAR2, MC_WINDOW};

// The DDR5 rows are per DRAM device; DDR4 trains one VREFDQ per rank, so on
// that generation the four collapse into the one row below.
pub const DDR4_DQ_VREF_PREFIX: &str = "DQ VREF D";
pub const DDR4_DQ_VREF_FIRST: &str = "DQ VREF D0";
pub const DDR4_DQ_VREF_NAME: &str = "DQ VREF";
pub const DDR4_DROP_MARKER: &str = "_drop_on_ddr4";

// VREF rows DDR4 has no counterpart for. DDR5 trains VREFCA and VREFCS in
// MR11 and MR12; DDR4 defines neither register, references the command bus
// against an external half-VDD supply and lets CS share it. Nothing on the
// module holds a value for either, so the rows come off rather than standing
// there restating that fact.
pub const DDR4_ABSENT_VREF_ROWS: [&str; 2] = ["CA VREF", "CS VREF"];

/// A base-taking reader as the shared form a row holds.
pub(crate) fn base_reader(f: impl Fn(u64) -> Option<String> + Send + Sync + 'static) -> BaseReaderFn {
    Arc::new(f)
}

impl Build {
    /// Use DDR-generation-correct VREF rows without CHA/CHB columns.
    pub(crate) fn install_platform_vref_additional(&mut self) {
        let generation = detect_ddr_generation();
        for timing in self.rows.iter_mut() {
            if !timing.is("Category", "VREF Additional") {
                continue;
            }
            // Remove the v21 dual-channel wrapper. Summary and Training should show one
            // clean value column for DQ/CA/CS VREF.
            timing.pop_all(&[
                "read_type_a",
                "read_type_b",
                "dynamic_params_a",
                "dynamic_params_b",
                "parameters_a",
                "parameters_b",
                "value_a",
                "value_b",
                "name_a",
                "name_b",
                "parameter_name",
            ]);

            let name = timing.name().unwrap_or_default().to_string();
            if generation == DdrGeneration::Ddr4 {
                timing.pop_all(&["dynamic_params", "address", "parameters", "Formula"]);
                timing.set_str("read_type", "standard");

                // DDR4 trains one VREFDQ per rank, not one per DRAM device, so the
                // four per-device rows collapse to a single reading. The first is
                // renamed and kept; the rest are marked for removal below.
                //
                // This branch matched a row called "DQ VREF" until the DDR5 work
                // split that row into D0-D3. Nothing then matched, so all four were
                // stripped of their source above and left permanently blank, while
                // the MR6 decoder that answers this on DDR4 went unused.
                if name.starts_with(DDR4_DQ_VREF_PREFIX) {
                    if name == DDR4_DQ_VREF_FIRST {
                        timing.set_str("name", DDR4_DQ_VREF_NAME);
                        timing.set("value", lazy(|| Some(decode_ddr4_dq_vref())));
                    } else {
                        timing.set("value", V::None);
                        timing.set(DDR4_DROP_MARKER, V::Bool(true));
                    }
                } else if DDR4_ABSENT_VREF_ROWS.contains(&name.as_str()) {
                    // DDR4 has no VREFCA and no VREFCS. The command/address
                    // reference is an external one at half VDD and CS rides on it,
                    // so neither is a mode-register field and neither is trained
                    // -- there is nothing on the module to read. These rows used
                    // to print "50.0% (fixed)" and "Uses CA VREF", which are not
                    // readings but the absence of one, written as though it were.
                    timing.set("value", V::None);
                    timing.set(DDR4_DROP_MARKER, V::Bool(true));
                }
                continue;
            }

            // DDR5 keeps the original MR10/MR11/MR12 dynamic readers.
            let original = match name.as_str() {
                "DQ VREF" => Some(dp(0xE600, 0x0A, 0xE200, 0, 7, 0xFEDC0000, 0, 0)),
                "CA VREF" => Some(dp(0xE600, 0x0B, 0xE200, 0, 7, 0xFEDC0000, 2, 0)),
                "CS VREF" => Some(dp(0xE600, 0x0C, 0xE200, 0, 7, 0xFEDC0000, 2, 0)),
                _ => None,
            };
            if let Some(params) = original {
                timing.pop("value");
                timing.set_str("read_type", "dynamic");
                timing.set("dynamic_params", V::Dyn(params));
                timing.set("Formula", V::Map(self.f.vref.clone()));
            }
        }
        let rows = std::mem::take(&mut self.rows);
        self.rows = rows
            .into_iter()
            .filter_map(|mut timing| match timing.pop(DDR4_DROP_MARKER) {
                Some(V::Bool(true)) => None,
                _ => Some(timing),
            })
            .collect();
    }

    /// The module-level statements between the VREF pass and the custom
    /// General section: Speed's value, the Timings -> Main rename, the
    /// General/Secondary fixes, and the old General rows' removal.
    pub(crate) fn normalise_main_rows(&mut self) {
        if let Some(timing) = self.rows.iter_mut().find(|timing| timing.is("name", "Speed")) {
            timing.set("value", text(get_speed()));
        }

        // --- UI customization: rename Timings tab to Main, remove CPU tab, and surface CPU/general info on Main.
        for timing in self.rows.iter_mut() {
            if timing.is("Tab", "Timings") {
                timing.set_str("Tab", "Main");
            }
        }

        // Normalize and fix specific General / Secondary rows.
        let platform = self.platform;
        for timing in self.rows.iter_mut() {
            let name = timing.name().unwrap_or_default().to_string();
            let main = timing.is("Tab", "Main");
            let general = timing.is("Category", "General");
            // The CPU and Motherboard rows, which the Python handled as two
            // branches doing the same thing.
            if (name == "CPU" || name == "Motherboard") && timing.is("Tab", "CPU") {
                timing.set_str("Tab", "Main");
                timing.set_str("Category", "General");
            } else if name == "Capacity" && main && general {
                timing.set_str("name", "Memory Capacity");
            } else if name == "Speed" && main && general {
                timing.set_str("name", "DRAM Frequency");
                timing.set("value", text(get_dram_frequency(platform)));
                timing.set("address", V::None);
                timing.set("parameters", no_bits());
                timing.set_str("read_type", "standard");
            } else if name == "Dram Ratio" && main && general {
                timing.set_str("name", "DRAM Ratio");
            } else if name == "Multiplier" && main && general {
                timing.set_str("name", "QCLK Ratio");
                timing.set("value", text(get_qclk_ratio()));
                timing.set("address", V::None);
                timing.set("parameters", no_bits());
                timing.set_str("read_type", "standard");
                timing.pop("Formula");
            } else if name == "tWR" && main && timing.is("Category", "Secondary") {
                timing.set("value", text(get_twr_value(None)));
                timing.set_str("read_type", "standard");
                timing.pop("dynamic_params");
            }
        }

        // Remove old CPU-tab rows that we are replacing with a custom ordered General section.
        self.rows.retain(|t| !(t.is("Tab", "Main") && t.is("Category", "General")));
    }

    /// The custom General section, put at the very front of Main so the order
    /// is exactly controlled.
    ///
    /// None of these are read on a timer. "live" is what puts a row on the
    /// refresh worker, and it belongs to the sensor rows alone -- those are
    /// readings that move continuously and have a window of their own that
    /// keeps their minimum, maximum and average. Everything on the reading
    /// tabs is a setting, shown as it was when the tab was drawn.
    ///
    /// The clocks and ratios hold their getter rather than the value it
    /// returned at import. That is not liveness: nothing calls them again on
    /// this tab. It means the Advanced window, which polls its own list, gets
    /// the current reading instead of a copy of startup.
    ///
    /// The identity rows above them hold a plain reading, because a CPU name,
    /// board, BIOS, microcode, module vendor and installed capacity cannot
    /// change while the machine runs and each one costs a WMI query.
    pub(crate) fn install_custom_general_rows(&mut self) {
        let platform = self.platform;
        let row = |name: &str, value: V| {
            T::new()
                .s("name", name)
                .with("value", value)
                .s("Category", "General")
                .s("Tab", "Main")
                .s("Column", "Left")
                .s("read_type", "standard")
        };
        let named = |name: &str, display: &str, value: V| row(name, value).s("display_name", display);
        let mut custom = vec![
            row("CPU", text(get_cpu_name())),
            row("Cores / Threads", text(get_cpu_cores_threads())),
            row("Manufacturer", text(get_board_manufacturer())),
            row("Model", text(get_board_model())),
            row("BIOS", text(get_bios_version())),
            row("Microcode", text(get_microcode())),
            named(
                "RAM Manufacturer",
                "Module Manuf.",
                lazy(|| match dimm_field("module_manufacturer") {
                    Some(name) if !name.is_empty() => Some(name),
                    _ => Some(get_ram_manufacturer()),
                }),
            ),
            named("Memory Capacity", "Capacity", text(get_total_physical_memory(platform))),
            row("DRAM Frequency", lazy(move || Some(get_dram_frequency(platform)))),
            row("CMD Stretch", text(get_cmd_stretch(None))),
            row("DRAM Ratio", lazy(|| Some(get_dram_ratio_value()))),
            row("Gear Mode", lazy(|| Some(get_gear_mode_value()))),
        ];
        custom.push(T::new()
            .s("name", "Channels")
            .with("value", opt(detect_dual_channel_memory(platform)))
            .s("Category", "General")
            .s("Tab", "Main")
            .with("parameters", no_bits())
            .s("Column", "Left")
            .s("read_type", "standard"));
        custom.extend([
            row("QCLK Ratio", lazy(|| Some(get_qclk_ratio()))),
            named("BCLK", "Bus Clock", lazy(move || Some(get_bclk_rd(platform)))),
            row("Core Ratio", lazy(get_core_ratio)),
            row("Uncore Ratio", lazy(move || get_uncore_ratio(platform))),
            row("Core Clock", lazy(get_core_clock)),
            named("Uncore", "Ring Clock", lazy(move || Some(get_ring_freq(platform)))),
            row("MCLK", lazy(|| Some(get_mclk()))),
            row("UCLK", lazy(|| Some(get_uclk()))),
            row("Power Down Mode", text(get_power_down_mode_value())),
            row("ECC / Error Correction", text(get_ecc_error_correction_value())),
            row("Self Refresh", text(get_self_refresh_value())),
            row("Memory Scrambler", text(get_memory_scrambler_value())),
            row("Row Hammer", text(get_row_hammer_value())),
            row("PSF0 PLL", lazy(|| Some(get_psf0_pll()))),
            row("VDDQ TX", text(get_tx())),
            row("VCCSA", text(get_sa())),
        ]);
        let rest = std::mem::take(&mut self.rows);
        custom.extend(rest);
        self.rows = custom;
    }

    /// --- Core Ultra 200S / Z890 live timing map.
    ///
    /// Switch Main-tab primary/secondary fields to the Core Ultra 200S register layout.
    pub(crate) fn install_arrow_lake_main_timings(&mut self) {
        if !is_arrow_lake_platform() {
            return;
        }
        // name: (offset, bit_start, bit_length)
        let field_map: [(&str, (u64, u32, i32)); 12] = [
            ("tRCD", (0xE138, 22, 8)),
            ("tRCDW", (0xE13C, 0, 8)),
            ("tRAS", (0xE004, 13, 9)),
            ("tRRD_L", (0xE138, 9, 6)),
            ("tRRD_S", (0xE138, 15, 7)),
            ("tFAW", (0xE138, 0, 9)),
            ("tRFC", (0xE4A0, 18, 13)),
            ("tRFCpb", (0xE4A4, 8, 11)),
            ("tREFI", (0xE4A0, 0, 18)),
            ("tRTP", (0xE000, 20, 7)),
            ("tWRPRE", (0xE004, 1, 10)),
            ("tREFIx9", (0xE4A4, 0, 8)),
        ];
        for timing in self.rows.iter_mut() {
            let name = timing.name().unwrap_or_default().to_string();
            if let Some((_, (offset, bit_start, bit_length))) = field_map.iter().find(|(n, _)| *n == name) {
                timing.set("address", V::Int(MCHBAR + offset));
                timing.set("parameters", bits(*bit_start, *bit_length));
                timing.set_str("read_type", "standard");
                timing.pop_all(&["value", "default_value", "dynamic_params", "Formula"]);
            } else if name == "tWR" && timing.is("Category", "Secondary") {
                timing.set("value", text(get_twr_value(None)));
                timing.set_str("read_type", "standard");
                timing.pop_all(&["address", "parameters", "dynamic_params", "Formula"]);
            } else if name == "tMOD" {
                timing.set("value", text(get_tmod_value()));
                timing.set_str("read_type", "standard");
                timing.pop_all(&["address", "parameters", "dynamic_params", "Formula"]);
            }
            // tCCD_L and tCCD_L_WR are not handled here any more. _install_ccd_
            // timings replaces those rows on every Intel platform, from the same
            // register fields this branch used, so doing it again here would only
            // spend two eager reads on a value that is then thrown away.
        }
    }

    /// Expose Intel's tRDPRE name using the active platform tRTP/TC_PRE field.
    pub(crate) fn install_trdpre_alias(&mut self) {
        if self.rows.iter().any(|item| item.is("name", "tRDPRE")) {
            return;
        }
        let Some(source) = self.rows.iter().find(|item| item.is("name", "tRTP")) else {
            return;
        };
        let mut alias = source.copy();
        alias.set_str("name", "tRDPRE");
        alias.set_str("Category", "Power down");
        alias.set_str("Tab", "Timings");
        alias.set_str("Column", "Left");
        self.rows.push(alias);
    }

    /// Put the requested power-down timings first and retain the rest's order.
    pub(crate) fn reorder_power_down_timings(&mut self) {
        const LEADING_ORDER: [&str; 6] = ["tCKE", "tXP", "tWRPRE", "tWRPDEN", "tRDPRE", "tRDPDEN"];
        let is_power_down = |t: &T| t.is("Tab", "Timings") && t.is("Category", "Power down");
        if !self.rows.iter().any(is_power_down) {
            return;
        }
        let rows = std::mem::take(&mut self.rows);
        let mut power_down: Vec<T> = Vec::new();
        let mut result: Vec<Option<T>> = Vec::new();
        let mut inserted = false;
        for timing in rows {
            if is_power_down(&timing) {
                if !inserted {
                    result.push(None);
                    inserted = true;
                }
                power_down.push(timing);
                continue;
            }
            result.push(Some(timing));
        }
        let leading_index = |name: Option<&str>| LEADING_ORDER.iter().position(|n| Some(*n) == name);
        let mut indexed: Vec<(usize, T)> = power_down.into_iter().enumerate().collect();
        indexed.sort_by_key(|(index, timing)| {
            let lead = leading_index(timing.name());
            (if lead.is_some() { 0 } else { 1 }, lead.unwrap_or(LEADING_ORDER.len()), *index)
        });
        let mut ordered = Some(indexed.into_iter().map(|(_, timing)| timing).collect::<Vec<T>>());
        for slot in result {
            match slot {
                Some(timing) => self.rows.push(timing),
                None => self.rows.extend(ordered.take().expect("inserted once")),
            }
        }
    }

    /// A per-channel Training row that reads both controllers on every refresh.
    ///
    /// Takes the reader rather than two readings. It used to take the values,
    /// which meant each row froze at whatever the DRAM had been told at startup
    /// -- fine for settings that never move, wrong the moment one does, and
    /// indistinguishable on screen from a live reading that happens to be steady.
    pub(crate) fn make_dual_live_row(&self, name: &str, category: &str, read: BaseReaderFn) -> T {
        let (base_a, base_b) = (MCHBAR, self.channel_b);
        let (read_a, read_b) = (read.clone(), read);
        // Empty parameter dictionaries intentionally mark this as a dual-column row;
        // main.py resolves the value_a/value_b getters.
        T::new()
            .s("name", name)
            .s("Category", category)
            .s("Tab", "Training")
            .s("Column", "Left")
            .s("parameter_name", "Name")
            .s("name_a", "CHA")
            .s("name_b", "CHB")
            .with("parameters_a", no_bits())
            .with("parameters_b", no_bits())
            .with("value_a", lazy(move || read_a(base_a)))
            .with("value_b", lazy(move || read_b(base_b)))
            .s("read_type_a", "standard")
            .s("read_type_b", "standard")
    }

    // --- DDR4 RTT / RON controller-shadow readers.
    pub(crate) fn install_ddr4_skew_live_rows(&mut self) {
        if detect_ddr_generation() != DdrGeneration::Ddr4 {
            return;
        }

        // Replace only RTT and RON. ODT and ODT timing are deliberately left alone.
        self.rows.retain(|timing| {
            !(timing.is("Tab", "Training") && matches!(timing.category(), Some("RTT") | Some("RON")))
        });

        let rtt_rows = vec![
            self.make_dual_live_row("RTT WR", "RTT", base_reader(|base| Some(get_ddr4_rtt_wr(base)))),
            self.make_dual_live_row("RTT NOM", "RTT", base_reader(|base| Some(get_ddr4_rtt_nom(base)))),
            self.make_dual_live_row("RTT PARK", "RTT", base_reader(|base| Some(get_ddr4_rtt_park(base)))),
        ];
        let ron_rows = vec![self.make_dual_live_row("RON", "RON", base_reader(|base| Some(get_ddr4_ron(base))))];

        // Preserve the original Training order: RTT, ODT, RON, then the remaining sections.
        let first_skew =
            self.rows.iter().position(|timing| timing.is("Tab", "Training")).unwrap_or(self.rows.len());
        let count = rtt_rows.len();
        self.rows.splice(first_skew..first_skew, rtt_rows);

        let mut after_odt = first_skew + count;
        while after_odt < self.rows.len()
            && self.rows[after_odt].is("Tab", "Training")
            && self.rows[after_odt].is("Category", "ODT")
        {
            after_odt += 1;
        }
        self.rows.splice(after_odt..after_odt, ron_rows);
    }

    // --- UI order adjustment for Main -> Secondary timings.
    pub(crate) fn reorder_main_secondary_timings(&mut self) {
        const DESIRED_ORDER: [&str; 10] =
            ["tWR", "tRFC", "tRFCpb", "tRRD_L", "tRRD_S", "tWTR_L", "tWTR_S", "tRTP", "tFAW", "tCWL"];
        let is_secondary = |t: &T| t.is("Tab", "Main") && t.is("Category", "Secondary");
        if !self.rows.iter().any(is_secondary) {
            return;
        }
        let rows = std::mem::take(&mut self.rows);
        let mut secondary: Vec<T> = Vec::new();
        let mut result: Vec<Option<T>> = Vec::new();
        let mut inserted = false;
        for timing in rows {
            if is_secondary(&timing) {
                if !inserted {
                    result.push(None);
                    inserted = true;
                }
                secondary.push(timing);
                continue;
            }
            result.push(Some(timing));
        }
        let mut indexed: Vec<(usize, T)> = secondary.into_iter().enumerate().collect();
        indexed.sort_by_key(|(index, timing)| {
            let desired = DESIRED_ORDER.iter().position(|n| Some(*n) == timing.name());
            (if desired.is_some() { 0 } else { 1 }, desired.unwrap_or(999), *index)
        });
        let mut ordered = Some(indexed.into_iter().map(|(_, timing)| timing).collect::<Vec<T>>());
        for slot in result {
            match slot {
                Some(timing) => self.rows.push(timing),
                None => self.rows.extend(ordered.take().expect("inserted once")),
            }
        }
    }

    // --- DDR4-aware ODT section.
    // The CA/CS/CK Group A/B fields are DDR5-only. DDR4's RTT rows already
    // report NOM/WR/PARK; do not duplicate them under DQ/DQS ODT labels.
    pub(crate) fn install_ddr4_odt_rows(&mut self) {
        if detect_ddr_generation() != DdrGeneration::Ddr4 {
            return;
        }
        // Remove the DDR5-only CA/CS/CK ODT rows that otherwise return N/A.
        self.rows.retain(|timing| !(timing.is("Tab", "Training") && timing.is("Category", "ODT")));
    }

    // --- Core Ultra 200S / Z890 VREF cleanup.
    // The legacy client-IMC drive-strength block used by Alder/Raptor Lake -- the
    // WrDS and RdODT rows at 0x2CE8/0x2CEC/0x2CF0 -- reads as 0xFFFFFFFF on Arrow
    // Lake. Do not present those invalid 255 values. Arrow Lake keeps the valid
    // DDR5 mode-register VREFDQ/VREFCA/VREFCS rows.
    pub(crate) fn install_arrow_lake_vref_rows(&mut self) {
        if !is_arrow_lake_platform() {
            return;
        }
        // The analog drive-strength rows used to be dropped here as obsolete on
        // this platform. They stay, by request, so Training carries the same rows on
        // both platforms and the ones that read nothing say N/A rather than
        // vanishing. This function now only promotes the DDR5 values beside them.
        // Present DQ/CA/CS as the normal VREF section on Arrow Lake rather than a
        // second "VREF Additional" panel.
        for timing in self.rows.iter_mut() {
            if timing.is("Tab", "Training")
                && timing.is("Category", "VREF Additional")
                && matches!(timing.name(), Some("DQ VREF") | Some("CA VREF") | Some("CS VREF"))
            {
                timing.set_str("Category", "VREF");
            }
        }
    }

    pub(crate) fn install_slew_rate_rows(&mut self) {
        // Remove an older copy when this file is reloaded during development.
        self.rows.retain(|timing| !(timing.is("Tab", "Training") && timing.is("Category", "Slew Rate")));

        // Every row on this tab holds a getter rather than a number. These registers
        // are live compensation results that retrain while the tool is open -- the
        // VssHiFF fields move between 48 and 49, and CLK's pull-down tracks them --
        // so a value read once at import would freeze at whatever the state happened
        // to be at startup and never move again. That is what made the tab appear to
        // disagree with the reference tools: both were right, ours was just older.
        let slew_row = |name: &str, value: V| {
            T::new()
                .s("name", name)
                .s("Category", slew_rate_category(name))
                .s("Tab", "Training")
                .s("Column", "Right")
                .with("value", value)
        };
        let field = |name: &str, offset: u64, bit_start: u32, bit_length: u32| {
            slew_row(name, lazy(move || Some(registers::read_slew_rate_field(offset, bit_start, bit_length))))
        };
        // A slew row whose base depends on the DDR generation.
        //
        // The generation is resolved once, here, and the reader it selects is what
        // gets called each refresh. Probing the generation on every read would put
        // a WMI round trip on the refresh path for a fact that cannot change while
        // the tool is running.
        let for_generation = |name: &str, offset: u64, bit_start: u32, bit_length: u32| {
            if detect_ddr_generation() == DdrGeneration::Ddr4 {
                slew_row(
                    name,
                    lazy(move || {
                        Some(registers::read_slew_rate_field_from_base(MCHBAR, offset, bit_start, bit_length))
                    }),
                )
            } else {
                slew_row(name, lazy(move || Some(registers::read_slew_rate_field(offset, bit_start, bit_length))))
            }
        };
        let clk = |name: &str, field_name: &'static str| {
            slew_row(name, lazy(move || Some(registers::read_ddr4_clk_slew_field(field_name))))
        };

        let slew_rows = vec![
            // DATA
            // DATA0CH0_CR_DDRCRDATACOMP0 is the per-channel register at 0x01B4.
            // The old 0x2CD8 address was inferred from the adjacent global
            // CMD/CLK/CTL compensation blocks; it returned plausible drive codes
            // but put both ODT codes one step high. The reference map and dump
            // pin this address and decode it as 52 / 42 / 17 / 14 / 48.
            field("Data Drv Up", 0x01B4, 0, 6),
            field("Data Drv Dn", 0x01B4, 6, 6),
            field("Data ODT Up", 0x01B4, 12, 6),
            field("Data ODT Dn", 0x01B4, 18, 6),
            for_generation("Data VssHiFFdq", 0x01B4, 24, 6),
            // CMD
            field("CMD Drv Up", 0x2CDC, 0, 6),
            field("CMD Drv Dn", 0x2CDC, 6, 6),
            field("CMD SComp", 0x2CDC, 12, 8),
            for_generation("CMD VssHiFF", 0x2CDC, 20, 6),
            // CLK - Z790 DDR4 trained result is read from MC1/CHB.
            clk("CLK Drv Up", "up"),
            clk("CLK Drv Dn", "dn"),
            clk("CLK SComp", "scomp"),
            clk("CLK VssHiFF", "vsshiff"),
            // CTL
            field("CTL Drv Up", 0x2CE4, 0, 6),
            field("CTL Drv Dn", 0x2CE4, 6, 6),
            field("CTL SComp", 0x2CE4, 12, 8),
            field("CTL VssHiFF", 0x2CE4, 20, 6),
            field("CTL CkeCsUp", 0x2CE4, 26, 6),
            // SComp diagnostics
            // One packed register rather than three scattered ones.
            //
            // These used to read from 0x2D00, 0x2CE8 and 0x2CF4. 0x2C24 holds them
            // in one tidy packed layout -- a flag then two byte fields -- and
            // decodes to 1 / 0 / 2 here, matching what the old addresses reported.
            // Three unrelated registers agreeing with one packed one is
            // coincidence; the packed one is the source, and it is what both
            // reference tools name.
            //
            // bonus20 and bonus19 are gone. Both tools place them at bits 22+3 and
            // 25+1 of this register, where they read 0 and 0, and neither is a
            // reading worth a row. bonus20 in particular is why: those tools show
            // 48 for it, but no window anywhere in 0x2C24 holds 48, and their own
            // map gives that row the id "ctlvsshiff" -- the same id as their CTL
            // VssHiFF row. They resolve by id, so the row renders CTL VssHiFF, and
            // CTL VssHiFF is 48. The number never came from this register at all.
            field("CMD SlewStatlegen", 0x2C24, 0, 1),
            field("SComp Code Live", 0x2C24, 2, 8),
            field("SComp Common Bonus", 0x2C24, 12, 8),
        ];

        // Place the new panel before PHY Control in the Training right column.
        let insert_at = self
            .rows
            .iter()
            .position(|timing| timing.is("Tab", "Training") && timing.is("Category", "PHY Control"))
            .unwrap_or(self.rows.len());
        self.rows.splice(insert_at..insert_at, slew_rows);
    }

    // --- Training-tab organization cleanup.
    // Keep the legacy analog VREF values and the DQ/CA/CS VREF values in one panel,
    // order WrDSCke CS Up after WrDSClk Dn, and place ODT DELAY below ODTL.
    pub(crate) fn organize_skew_sections(&mut self) {
        const VREF_NAME_ORDER: [&str; 20] = [
            "WrDS Up",
            "WrDS Dn",
            "RdODT Up",
            "RdODT Dn",
            "WrDSCmd Up",
            "WrDSCmd Dn",
            "WrDSCtl Up",
            "WrDSCtl Dn",
            "WrDSClk Up",
            "WrDSClk Dn",
            // CKE/CS is the one rail with an up level and no down level. See the
            // note by the VREF rows: nothing maps a CKE/CS pull-down, and BIOS
            // offers no setting for one.
            "WrDSCke CS Up",
            // The two that are not up/down pairs, kept together directly under the
            // pairs rather than trailing the per-device levels below them.
            "RX VREF",
            "QXCOUNT",
            // DDR5 reports a level per DRAM device; DDR4 trains one per rank and
            // collapses to the single row. Both spellings are listed so the
            // generation that is not running does not sort its row to the end.
            "DQ VREF",
            "DQ VREF D0",
            "DQ VREF D1",
            "DQ VREF D2",
            "DQ VREF D3",
            "CA VREF",
            "CS VREF",
        ];

        let is_vref = |t: &T| t.is("Tab", "Training") && matches!(t.category(), Some("VREF") | Some("VREF Additional"));
        if let Some(first_vref_index) = self.rows.iter().position(is_vref) {
            let rows = std::mem::take(&mut self.rows);
            let (vref_rows, rest) = partition(rows, is_vref);
            let mut indexed: Vec<(usize, T)> = vref_rows.into_iter().enumerate().collect();
            for (_, timing) in indexed.iter_mut() {
                timing.set_str("Category", "VREF");
                timing.set_str("Column", "Middle");
            }
            indexed.sort_by_key(|(original, timing)| {
                let order = VREF_NAME_ORDER.iter().position(|n| Some(*n) == timing.name()).unwrap_or(VREF_NAME_ORDER.len());
                (order, *original)
            });
            self.rows = rest;
            self.rows.splice(first_vref_index..first_vref_index, indexed.into_iter().map(|(_, timing)| timing));
        }

        let is_odt_delay = |t: &T| t.is("Tab", "Training") && t.is("Category", "ODT DELAY");
        if self.rows.iter().any(is_odt_delay) {
            let rows = std::mem::take(&mut self.rows);
            let (mut odt_delay_rows, rest) = partition(rows, is_odt_delay);
            self.rows = rest;
            for timing in odt_delay_rows.iter_mut() {
                timing.set_str("Column", "Left");
            }
            let insert_at = self
                .rows
                .iter()
                .rposition(|timing| timing.is("Tab", "Training") && timing.is("Category", "ODTL"))
                .map(|index| index + 1)
                .unwrap_or(self.rows.len());
            self.rows.splice(insert_at..insert_at, odt_delay_rows);
        }
    }

    // --- DDR4 ODT latency values.
    // The old ODTL reader searched a legacy MRC command table that is not present
    // on the tested Z790 DDR4 memory-controller dump, which produced N/A or stale
    // offset codes.  For DDR4, expose the actual JEDEC latencies derived from the
    // active controller tCL/tCWL values (desktop UDIMM defaults: AL=0, PL=0, 1tCK
    // preamble).  Keep the existing trained-offset reader on DDR5 platforms.
    pub(crate) fn install_ddr4_odtl_values(&mut self) {
        if detect_ddr_generation() != DdrGeneration::Ddr4 {
            return;
        }
        let is_odtl = |t: &T| t.is("Tab", "Training") && t.is("Category", "ODTL");
        let Some(insert_at) = self.rows.iter().position(is_odtl) else {
            return;
        };
        self.rows.retain(|timing| !is_odtl(timing));

        let odtl_row = |name: &str, kind: &'static str| {
            T::new()
                .s("name", name)
                .with("value", lazy(move || Some(registers::ddr4_odtl_clocks(kind))))
                .s("Category", "ODTL")
                .s("Tab", "Training")
                .s("Column", "Left")
                .s("read_type", "standard")
        };
        let replacement_rows = vec![
            odtl_row("ODTL WR ON", "write_on"),
            odtl_row("ODTL WR OFF", "write_off"),
            odtl_row("ODTL WR NT ON", "write_on"),
            odtl_row("ODTL WR NT OFF", "write_off"),
            odtl_row("ODTL RD NT ON", "read_on"),
            odtl_row("ODTL RD NT OFF", "read_off"),
        ];
        self.rows.splice(insert_at..insert_at, replacement_rows);
    }

    // --- DDR4 refresh-mode fix.
    // Z790 DDR4 does not expose the old MRC command-table record used by the
    // original dynamic reader. Replace only that row with Intel DDR_PTM_CTL[3:2].
    pub(crate) fn install_ddr4_refresh_mode_value(&mut self) {
        if detect_ddr_generation() != DdrGeneration::Ddr4 {
            return;
        }
        if let Some(timing) = self.rows.iter_mut().find(|timing| {
            timing.is("Tab", "Training") && timing.is("Category", "REFRESH MODE") && timing.is("name", "REFRESH")
        }) {
            timing.m.clear();
            timing.set_str("name", "REFRESH");
            timing.set("value", lazy(|| Some(get_ddr4_refresh_mode_value())));
            timing.set_str("Category", "REFRESH MODE");
            timing.set_str("Tab", "Training");
            timing.set_str("Column", "Left");
            timing.set_str("read_type", "standard");
        }
    }

    // --- v46 tab organization.
    // Keep all platform-specific installers working with the historical "Main"
    // label, then remap the completed rows once the timing table is finalized.
    pub(crate) fn install_system_info_and_timings_tabs(&mut self) {
        for timing in self.rows.iter_mut() {
            if !timing.is("Tab", "Main") {
                continue;
            }
            if timing.is("Category", "General") {
                timing.set_str("Tab", "System Info");
            } else {
                timing.set_str("Tab", "Timings");
            }
        }
    }

    pub(crate) fn install_arrow_lake_power_down_rows(&mut self) {
        if !is_arrow_lake_platform() {
            return;
        }
        for timing in self.rows.iter_mut() {
            let name = timing.name().unwrap_or_default().to_string();
            let wide = ARROW_LAKE_POWER_DOWN_WIDE.iter().find(|(n, _)| *n == name);
            let standard = ARROW_LAKE_POWER_DOWN.iter().find(|(n, _)| *n == name);
            if let Some((_, (offset, bit_start, bit_length))) = wide.or(standard) {
                timing.set("address", V::Int(MCHBAR + offset));
                timing.set("parameters", bits(*bit_start, *bit_length));
                timing.set_str("read_type", if wide.is_some() { "wide" } else { "standard" });
                timing.pop_all(&["value", "Formula"]);
            } else if ARROW_LAKE_POWER_DOWN_UNKNOWN.contains(&name.as_str()) {
                timing.set("value", text("N/A"));
                timing.set_str("read_type", "standard");
                timing.pop_all(&["address", "parameters", "Formula"]);
            }
        }
    }

    // --- Derived refresh row.
    // tRFCns is computed from the live tRFC and MCLK, mirroring AM5, and is
    // pinned directly above tRFC so Summary and Timings share one order.
    pub(crate) fn install_trfc_ns_row(&mut self) {
        let Some(index) = self
            .rows
            .iter()
            .position(|timing| timing.is("name", "tRFC") && timing.is("Category", "Refresh timings"))
        else {
            return;
        };
        if self.rows.iter().any(|timing| timing.is("name", "tRFCns")) {
            return;
        }
        let reference = &self.rows[index];
        let tab = reference.tab().unwrap_or("Timings").to_string();
        let column = reference.column().unwrap_or("Left").to_string();
        let getter = self.trfc_ns_reader();
        let row = T::new()
            .s("name", "tRFCns")
            .with("value", lazy(move || getter(None)))
            .s("Category", "Refresh timings")
            .s("Tab", &tab)
            .s("Column", &column)
            .s("read_type", "standard");
        self.rows.insert(index, row);
    }

    /// Pin tREFIns directly below the raw interval it restates.
    pub(crate) fn install_trefi_ns_row(&mut self) {
        let Some(index) = self
            .rows
            .iter()
            .position(|timing| timing.is("name", "tREFI") && timing.is("Category", "Refresh timings"))
        else {
            return;
        };
        if self.rows.iter().any(|timing| timing.is("name", "tREFIns")) {
            return;
        }
        let reference = &self.rows[index];
        let tab = reference.tab().unwrap_or("Timings").to_string();
        let column = reference.column().unwrap_or("Left").to_string();
        let getter = self.trefi_ns_reader();
        let row = T::new()
            .s("name", "tREFIns")
            .with("value", lazy(move || getter(None)))
            .s("Category", "Refresh timings")
            .s("Tab", &tab)
            .s("Column", &column)
            .s("read_type", "standard");
        self.rows.insert(index + 1, row);
    }

    /// Put the four DFE taps on the Training tab, directly below ODT DELAY.
    ///
    /// Per channel: the mode-register table exists in both controller windows and
    /// the taps genuinely differ between them, so each row reads its own side
    /// rather than showing one controller's training twice.
    ///
    /// DDR4 gets no rows at all. Decision feedback equalisation is a DDR5 feature
    /// -- the equaliser sits in the DRAM's DQ receiver and is configured through
    /// mode registers that DDR4 does not define -- so there is nothing to read
    /// rather than something that failed to read. Measured on the Z790-P DDR4
    /// bench, the mode-register window is empty in any case: not one entry in the
    /// whole 0xE600-0xE800 table, which is also why tCCD_L reports nothing there.
    pub(crate) fn install_dfe_rows(&mut self) {
        // Scoped to this tab. Removing every row categorised DFE also took the
        // Jedec tab's global rows, which are a different section that happens to
        // share the name -- and took them before anything could decide whether to
        // keep them.
        self.rows.retain(|timing| !(timing.is("Tab", "Training") && timing.is("Category", "DFE")));

        if detect_ddr_generation() == DdrGeneration::Ddr4 {
            return;
        }

        let mut anchor = None;
        let mut column = "Right".to_string();
        for (index, timing) in self.rows.iter().enumerate() {
            if timing.is("Tab", "Training") && timing.is("Category", "ODT DELAY") {
                anchor = Some(index + 1);
                column = timing.column().map(str::to_string).unwrap_or(column);
            }
        }

        let channel_b = self.channel_b;
        let mut rows = Vec::new();
        for tap in 1..=DFE_TAP_COUNT {
            type Getter = fn(u64, Option<u64>) -> Option<String>;
            for (suffix, getter) in [("Enable", get_dfe_enable as Getter), ("Bias", get_dfe_bias as Getter)] {
                rows.push(
                    T::new()
                        .s("name", &format!("DFE Tap {tap} {suffix}"))
                        .with("value_a", lazy(move || getter(tap, Some(MCHBAR))))
                        .with("value_b", lazy(move || getter(tap, Some(channel_b))))
                        .s("Category", "DFE")
                        .s("Tab", "Training")
                        .s("Column", &column)
                        .s("read_type", "standard"),
                );
            }
        }
        match anchor {
            None => self.rows.extend(rows),
            Some(anchor) => {
                self.rows.splice(anchor..anchor, rows);
            }
        }
    }

    // The Jedec tab held nothing but DFE rows -- the per-tap enables and values,
    // and the global gain -- and the taps now sit on the Training tab beside the VREF
    // levels they bias. A whole tab for one section that lives somewhere else is a
    // second place to look for the same thing, so it goes.
    //
    // The gain is the exception. It is not a tap, so the per-tap rows above do not
    // cover it, and dropping the tab would have taken the only reading of it with
    // them. These two move rather than being rebuilt, so their decode and formulas
    // come across exactly as they were.
    pub(crate) fn remove_jedec_tab(&mut self) {
        // The gain describes the same DDR5 equaliser the taps do, so where the taps
        // are not installed it is not carried across either -- it would be the only
        // DFE row on a generation that has no DFE.
        let keep_dfe = detect_ddr_generation() != DdrGeneration::Ddr4;

        let mut kept = Vec::new();
        let mut remaining = Vec::new();
        let mut column = "Right".to_string();
        let mut last_dfe = None;
        for timing in std::mem::take(&mut self.rows) {
            if timing.is("Tab", "Training") && timing.is("Category", "DFE") {
                column = timing.column().map(str::to_string).unwrap_or(column);
            }
            if !timing.is("Tab", JEDEC_TAB) {
                let dfe = timing.is("Tab", "Training") && timing.is("Category", "DFE");
                remaining.push(timing);
                if dfe {
                    last_dfe = Some(remaining.len());
                }
                continue;
            }
            let label = if keep_dfe {
                JEDEC_ROWS_KEPT.iter().find(|(name, _)| Some(*name) == timing.name()).map(|(_, label)| *label)
            } else {
                None
            };
            let Some(label) = label else {
                continue;
            };
            let mut moved = timing.copy();
            moved.set_str("name", label);
            moved.set_str("Tab", "Training");
            moved.set_str("Category", "DFE");
            moved.set_str("Column", &column);
            kept.push(moved);
        }
        match last_dfe {
            None => remaining.extend(kept),
            Some(at) => {
                remaining.splice(at..at, kept);
            }
        }
        self.rows = remaining;
    }

    /// Put PHY Control directly after VREF in the middle Training column.
    ///
    /// They are the leftovers of the tab -- bandwidth select, the DCC code, the
    /// ODT enables -- and belong beside the VREF state they supplement rather
    /// than at the foot below the unrelated compensation and DFE groups.
    pub(crate) fn place_misc_additional_after_vref(&mut self) {
        let is_misc = |t: &T| t.is("Tab", "Training") && t.is("Category", "PHY Control");
        if !self.rows.iter().any(is_misc) {
            return;
        }
        let rows = std::mem::take(&mut self.rows);
        let (mut misc, mut rest) = partition(rows, is_misc);
        let anchor = rest
            .iter()
            .rposition(|timing| timing.is("Tab", "Training") && timing.is("Category", "VREF"))
            .map(|index| index + 1);
        for timing in misc.iter_mut() {
            timing.set_str("Column", "Middle");
        }
        match anchor {
            None => rest.extend(misc),
            Some(anchor) => {
                rest.splice(anchor..anchor, misc);
            }
        }
        self.rows = rest;
    }

    /// Install the column-to-column delays as one dedicated section.
    pub(crate) fn install_ccd_timings(&mut self) {
        for name in SUPERSEDED_CCD_ROWS {
            self.rows.retain(|timing| !timing.is("name", name));
        }
        for name in CCD_ROWS {
            self.rows.push(
                T::new()
                    .s("name", name)
                    .with("value", lazy(move || get_ccd_timing(name, None).map(|v| v.to_string())))
                    .s("Category", CCD_CATEGORY)
                    .s("Tab", "Timings")
                    .s("Column", "Right")
                    .s("read_type", "standard"),
            );
        }
    }

    /// Header text for the two value columns, named for the slots actually
    /// populated on each controller rather than assumed to be the first of each
    /// pair. The board reports its own slot names, and on this target they are A2
    /// and B2, not A1 and B1 - so a fixed label would have named the wrong sockets
    /// and disagreed with the module strip along the bottom of the window.
    ///
    /// A channel with two modules is named for both, "A1/A2". A channel whose slot
    /// the board did not name falls back to the controller, which is what the column
    /// always really is.
    pub(crate) fn resolve_channel_labels(&mut self) {
        let channels =
            crate::memory::dimm_inventory::slots_by_channel(&crate::memory::dimm_inventory::read_modules(false));
        let label = |letter: &str, fallback: &str| {
            let joined = channels.get(letter).map(|slots| slots.join("/")).unwrap_or_default();
            if joined.is_empty() { fallback.to_string() } else { joined }
        };
        self.channel_labels = (label("A", "ChA"), label("B", "ChB"));
    }

    /// Label the two value columns and blank the header over the name gutter.
    pub(crate) fn dual_channel_headers(&self, timing: &mut T) {
        timing.set_str("parameter_name", "Name");
        timing.set_str("name_a", &self.channel_labels.0);
        timing.set_str("name_b", &self.channel_labels.1);
    }

    /// Return the channel-B twin of a channel-A address, or `None` if there is none.
    ///
    /// How far the twin sits depends on the platform, which is why the distance
    /// comes from CHANNEL_B_OFFSET rather than being written here: the second
    /// controller on Raptor Lake, DDR4 and DDR5 alike, and a sub-channel step on
    /// Core Ultra 200S. See the note beside it.
    ///
    /// Callers decide which registers have a twin at all -- the 0xE000 block,
    /// where the RTL rows establish it, and on Raptor Lake DDR5 the 0xD800
    /// controller block too; see _has_channel_b_copy. An address already in
    /// channel B, one a platform pass cleared to None, and anything outside the
    /// window all return None so the caller leaves the row alone.
    pub(crate) fn channel_b_address(&self, address: Option<u64>) -> Option<u64> {
        let address = address?;
        if !(MCHBAR..MCHBAR + MC_WINDOW).contains(&address) {
            return None;
        }
        if (address - MCHBAR) & self.channel_b_offset != 0 {
            return None;
        }
        Some(address + self.channel_b_offset)
    }

    /// Mirror a plain address+bitfield row onto the second controller.
    ///
    /// "Standard" here means the row shape -- an address and a bitfield -- and
    /// not the read width, which the two senses of the word used to conflate.
    /// Both sides carried a literal "standard" read type, so a row asking for the
    /// 64-bit read got mirrored into two 4-byte reads and the display showed a
    /// field truncated at the dword boundary while the register table read it
    /// correctly. The declared type travels with the row now.
    pub(crate) fn promote_standard_row(&self, timing: &mut T) -> bool {
        let Some(address_b) = self.channel_b_address(timing.int("address")) else {
            return false;
        };
        let Some(parameters) = timing.params("parameters") else {
            return false;
        };
        let read_type = match timing.str("read_type") {
            Some(kind) if DIRECT_READ_TYPES.contains(&kind) => kind.to_string(),
            _ => "standard".to_string(),
        };
        let address = timing.int("address").expect("address checked above");
        timing.set("address_a", V::Int(address));
        timing.set("address_b", V::Int(address_b));
        timing.set("parameters_a", V::Params(Some(parameters)));
        timing.set("parameters_b", V::Params(Some(parameters)));
        timing.set_str("read_type_a", &read_type);
        timing.set_str("read_type_b", &read_type);
        self.dual_channel_headers(timing);
        true
    }

    /// Mirror a dynamic-search row by pointing its copy at the second MCHBAR.
    pub(crate) fn promote_dynamic_row(&self, timing: &mut T) -> bool {
        let Some(params) = timing.dynamic("dynamic_params") else {
            return false;
        };
        if params.mchbar != MCHBAR {
            return false;
        }
        let params_b = crate::hardware::read::DynamicParams { mchbar: self.channel_b, ..params };
        timing.set("dynamic_params_a", V::Dyn(params));
        timing.set("dynamic_params_b", V::Dyn(params_b));
        timing.set_str("read_type_a", "dynamic");
        timing.set_str("read_type_b", "dynamic");
        self.dual_channel_headers(timing);
        true
    }

    /// Give a computed row one getter call per controller.
    pub(crate) fn promote_computed_row(&self, timing: &mut T, getter: BaseReaderFn) -> bool {
        let (fn_a, fn_b) = (getter.clone(), getter);
        let channel_b = self.channel_b;
        timing.set("value_a", lazy(move || fn_a(MCHBAR)));
        timing.set("value_b", lazy(move || fn_b(channel_b)));
        self.dual_channel_headers(timing);
        true
    }

    /// Rows with no address to mirror. Each side names the getter to call for its
    /// controller, bound per read rather than at import, so channel B is read the
    /// same way channel A is.
    pub(crate) fn dual_channel_computed(&self, name: &str) -> Option<BaseReaderFn> {
        Some(match name {
            "tRC" => base_reader(|base| Some(get_trc_value(Some(base)))),
            "CR" => base_reader(|base| Some(get_command_rate(Some(base)))),
            "tWTR_L" => base_reader(|base| Some(get_twtr_l(Some(base)))),
            "tWTR_S" => base_reader(|base| Some(get_twtr_s(Some(base)))),
            // Derived rather than read: the platform passes replace both of these with
            // a getter and drop the address, so there is nothing for the address
            // mirror to work from.
            "tWR" => base_reader(|base| Some(get_twr_value(Some(base)))),
            "tRFCns" => {
                let getter = self.trfc_ns_reader();
                base_reader(move |base| getter(Some(base)))
            }
            "tREFIns" => {
                let getter = self.trefi_ns_reader();
                base_reader(move |base| getter(Some(base)))
            }
            // Same shape: installed as getters just above, over a register field the
            // mirror cannot see because the row carries no address.
            "tCCD" | "tCCD_L" | "tCCD_L_WR" | "tCCD_L_WR2" => {
                let name = CCD_ROWS.iter().copied().find(|n| *n == name).expect("listed");
                base_reader(move |base| get_ccd_timing(name, Some(base)).map(|v| v.to_string()))
            }
            "tDLLK" => base_reader(|base| get_dllk_timing(Some(base)).map(|v| v.to_string())),
            _ => return None,
        })
    }

    pub(crate) fn install_dual_channel_timings(&mut self) {
        let mut rows = std::mem::take(&mut self.rows);
        for timing in rows.iter_mut() {
            if !timing.is("Tab", DUAL_CHANNEL_TAB) {
                continue;
            }
            if timing.category().is_some_and(|c| DUAL_CHANNEL_SKIP_CATEGORIES.contains(&c)) {
                continue;
            }
            if is_dual_timing(timing) {
                // A row that already reads both sides its own way keeps doing so.
                continue;
            }
            let name = timing.name().unwrap_or_default().to_string();
            if let Some(computed) = self.dual_channel_computed(&name) {
                self.promote_computed_row(timing, computed);
                continue;
            }
            if timing.is("read_type", "dynamic") {
                self.promote_dynamic_row(timing);
                continue;
            }
            self.promote_standard_row(timing);
        }
        self.rows = rows;
    }

    /// Read the second column of the hand-written MR rows from channel B.
    ///
    /// The RTT, ODT, drive-strength and DFE rows on Training were written with
    /// both sides spelled out, the second at MCHBAR2. On Raptor Lake, DDR4 and
    /// DDR5 alike, CHANNEL_B is MCHBAR2 -- B1's controller; see the note at
    /// CHANNEL_B_OFFSET -- so nothing there changes. This only moves them on a
    /// platform whose second module is a sub-channel step away instead.
    pub(crate) fn point_mode_register_rows_at_channel_b(&mut self) {
        let channel_b = self.channel_b;
        for timing in self.rows.iter_mut() {
            let (Some(params_a), Some(params_b)) = (timing.dynamic("dynamic_params_a"), timing.dynamic("dynamic_params_b"))
            else {
                continue;
            };
            if params_a.mchbar == MCHBAR && params_b.mchbar == MCHBAR2 {
                timing.set("dynamic_params_b", V::Dyn(crate::hardware::read::DynamicParams { mchbar: channel_b, ..params_b }));
            }
        }
    }

    /// `TIMINGS.remove(row)` by identity, for passes that hold a row's id.
    pub(crate) fn take_row(&mut self, id: u64) -> Option<T> {
        remove_id(&mut self.rows, id)
    }

    pub(crate) fn position_of(&self, id: u64) -> Option<usize> {
        index_of(&self.rows, id)
    }

    /// Stable-sort the rows `select` picks among their own positions.
    pub(crate) fn sort_rows_where<K: Ord>(&mut self, select: impl Fn(&T) -> bool, key: impl Fn(&T) -> K) {
        let positions: Vec<usize> =
            self.rows.iter().enumerate().filter(|(_, timing)| select(timing)).map(|(i, _)| i).collect();
        sort_in_place(&mut self.rows, &positions, key);
    }
}

// --- Core Ultra 200S power-down fields.
// 0xE050 carries tRDPDEN and tCPDED at different bit positions than
// Alder/Raptor Lake, confirmed by diffing full MCHBAR snapshots across a BIOS
// change (tRDPDEN 34->61, tCPDED 22->23) and reproduced identically in all
// four channel copies at 0xE050/0xE850/0xF050/0x1E050.
//
// tWRPDEN was previously N/A here, recorded as "nothing tracked 90->123".
// That conclusion was an artefact of how the register was being read, not a
// property of the silicon. The field is bits 27..36 -- it straddles the 32-bit
// boundary, so no search over dword-local bit positions could ever match it at
// any width. Read as one 64-bit quantity it is exact: MCHBAR snapshots taken
// either side of a BIOS change decode to 90 and 120 against BIOS 90 and 120,
// and the straddle is visible in the raw pair, where 0xE050 moves
// 0xD0658204 -> 0xC0658204 and 0xE054 moves 0x2206EA02 -> 0x2206EA03. Both
// halves carry part of the value, which is why neither dword alone shows it.
//
// tPRPDEN stays N/A. Bits 59..63 read 4 and decode coherently alongside the
// rest of the register, but the value did not change across the one BIOS pair
// on hand, so it has a plausible position and no confirmation. A row that
// reads nothing beats a row that reads a guess.
//
// tXPDLL is N/A here for a stronger reason than "not found": the bits it was
// reading belong to something else. The base table has it at 0xE050 bits
// 14..20, which is where this platform keeps tCPDED -- confirmed by a BIOS
// diff on this bench, and agreed by the reference layout, which has no tXPDLL
// at this register at all. Left alone the two rows read the same bits and
// printed the same number under different names, so the row was not merely
// unverified, it was tCPDED wearing a second label.
//
// tPPD moves for the same reason tXPDLL was blanked: the base position,
// 0xE000 bits 20..23, is the low nibble of the tRTP field, and the two rows
// read the same 12. The reference layout puts tPPD at bits 28..31 of the same
// register, where it reads 2 -- which is what that tool displays for the row
// on this board on the same boot, against the 12 the old position yields.
pub const ARROW_LAKE_POWER_DOWN: &[(&str, (u64, u32, i32))] =
    &[("tRDPDEN", (0xE050, 19, 6)), ("tCPDED", (0xE050, 14, 5)), ("tPPD", (0xE000, 28, 4))];
//
// tCKCKEH, tSR and tXSDLL move here for the same reason. Each sits above bit
// 32, so the base table reaches them by pointing at the upper dword and
// subtracting 32 from the bit position -- which works for a field that lies
// wholly above the boundary, and is why only tWRPDEN was ever missing rather
// than all four. On this platform they are at different bit positions, not
// different registers, so the override is a position change and the read type
// follows from it.
//
// The positions come from the reference tool's own register map, which carries
// two layouts. The one used here is identified rather than assumed: it agrees
// with all three Core Ultra 200S positions already established on this bench by
// BIOS diffs -- tCPDED at 14, tRDPDEN at 19, tWRPDEN at 27 -- while the other
// layout matches the base table above at every one of them (tXPDLL at 14,
// tRDPDEN at 21, tWRPDEN in the upper dword). Two internally consistent
// layouts, and the four points we had measured ourselves pick one of them.
//
// That same layout puts tXSR at 0xE4C0 bits 0..12, exactly where the base table
// already has it, so tXSR needs no override -- a fourth agreement, and the one
// that costs nothing to check. It also has no 0xE440 or 0xE448 block at all,
// which is consistent with tMOD reading nothing here however it is addressed.
//
// Unlike tWRPDEN these three have no two-value BIOS test behind them, but they
// are not resting on the map alone: read at these positions they return 2816,
// 16 and 45, which are the three values the reference tool displays for them on
// this board on the same boot. A map that has been right at four independently
// measured points, and three same-boot readings that agree with it, is a
// different standard of evidence from a plausible-looking field.
//
// tCKCKEH is the clearest gain regardless: the base position reads a flat 0
// here, so the row was showing a wrong number rather than an honest blank.
// Fields needing the 64-bit read, as (offset, bit_start, bit_length).
pub const ARROW_LAKE_POWER_DOWN_WIDE: &[(&str, (u64, u32, i32))] = &[
    ("tWRPDEN", (0xE050, 27, 10)),
    ("tCKCKEH", (0xE050, 37, 5)),
    ("tSR", (0xE4C0, 45, 6)),
    ("tXSDLL", (0xE4C0, 51, 13)),
];
pub const ARROW_LAKE_POWER_DOWN_UNKNOWN: [&str; 2] = ["tPRPDEN", "tXPDLL"];

// --- Column-to-column delays.
//
// tCCD_L and tCCD_L_WR used to be read by searching the mode-register window
// for a DDR5 MR pattern and decoding the hit through a JEDEC MR table. That
// search finds nothing on DDR4, and its zero result indexed the tables' first
// entry, so tCCD_L printed a constant 8 that happened to be right and
// tCCD_L_WR printed 32, a DDR5 figure with no meaning on DDR4. Neither was a
// reading, so both are gone.
//
// What replaces them is not yet a reading either: see CCD_CONFIRMED_FIELDS for
// which register field was tried, how a controlled BIOS change refuted it, and
// what would confirm the remaining candidate. Until then these rows report
// nothing, rather than the number the refuted mapping would print.
//
// The delay the controller actually applies is not hidden by this: it is on
// the same tab as tRDRD_sg, tRDRD_dg and tWRWR_sg.
//
// Runs before the channel-B pass so these rows gain the same A1/B1 columns as
// the rest of the tab.

// tCCD leads the group, as the BIOS lists it. It is the different-bank-group
// delay, and unlike tCCD_L it is not programmable on either generation: DDR4
// fixes it at 4 nCK and DDR5 at 8. See DDR4_TCCD_S for why it is still keyed
// on a register read rather than written as a bare constant, and why the row
// is empty on a generation this project has no burst-length read for.
pub const CCD_ROWS: [&str; 4] = ["tCCD", "tCCD_L", "tCCD_L_WR", "tCCD_L_WR2"];

// What the mode-register search left behind, in the order they appeared.
pub const SUPERSEDED_CCD_ROWS: [&str; 2] = ["tCCDL", "tCCDL WR"];

// The delays have their own section so the complete family stays together.
pub const CCD_CATEGORY: &str = "CAS to CAS";

pub const JEDEC_TAB: &str = "Jedec";

pub const JEDEC_ROWS_KEPT: &[(&str, &str)] = &[("Global DFE Gain", "DFE Gain Enable"), ("DFE GAIN Value", "DFE Gain")];

// --- Channel B columns on the Timings tab.
//
// Every row above reads the first memory controller at MCHBAR, so the tab has
// only ever shown channel A. The second controller repeats the same register
// block one 0x10000 window higher: the RTL rows already depend on that when
// they label MC0 as A1/A2 and MC1 as B1/B2, and the Arrow Lake power-down
// survey found tRDPDEN identical at 0xE050 and 0x1E050, which is that same
// window delta. Promoting a row therefore means reading one offset from both
// controllers and letting the section draw the ChA/ChB columns main.py already
// renders for AM5.
//
// This runs after every other installer so it mirrors the addresses those
// passes finalised rather than the ones the table literal declared. Anything it
// cannot mirror from a confirmed MC0 address is left single, so an unmirrored
// row shows one value instead of a register that was never verified to hold
// that timing.
pub const DUAL_CHANNEL_TAB: &str = "Timings";

// Capacity, Speed and Channels describe the system, and the ratio, gear and
// stretch rows describe a configuration both controllers run. A second column
// would repeat the first rather than tell a tuner anything, so these stay
// single and the section renders them value-then-blank.
pub const DUAL_CHANNEL_SKIP_CATEGORIES: [&str; 1] = ["General"];

// Read widths a mirrored row may carry across to its channel-B copy.
pub const DIRECT_READ_TYPES: [&str; 2] = ["standard", "wide"];
