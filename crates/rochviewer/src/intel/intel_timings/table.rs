//! The table as declared, before any pass has touched it: the Python's
//! `TIMINGS = [...]` literal, row for row and in order.

use std::sync::Arc;

use crate::hardware::read::DynamicParams;

use super::build::Build;
use super::dict::{T, V, bits, lazy, no_bits, opt, text};
use super::identity::{detect_dual_channel_memory, get_cpu_name, get_motherboard_name, get_total_physical_memory};
use super::registers::{
    get_bclk_rd, get_cmd_stretch, get_command_rate, get_dllk_timing, get_imc_freq, get_ring_freq, get_sa,
    get_trc_value, get_twtr_l, get_twtr_s, get_tx,
};
use super::{MCHBAR, MCHBAR2};

type Map = Arc<std::collections::BTreeMap<u64, String>>;

/// A register field row: `{"name", "address": MCHBAR + offset, "Category",
/// "Tab", "parameters": {...}, "Column", "read_type": "standard"}`.
fn reg(name: &str, offset: u64, category: &str, tab: &str, bit_start: u32, bit_length: i32, column: &str) -> T {
    T::new()
        .s("name", name)
        .with("address", V::Int(MCHBAR + offset))
        .s("Category", category)
        .s("Tab", tab)
        .with("parameters", bits(bit_start, bit_length))
        .s("Column", column)
        .s("read_type", "standard")
}

/// The same with a formula.
#[allow(clippy::too_many_arguments)]
fn reg_f(
    name: &str,
    offset: u64,
    category: &str,
    tab: &str,
    bit_start: u32,
    bit_length: i32,
    column: &str,
    formula: Map,
) -> T {
    reg(name, offset, category, tab, bit_start, bit_length, column).with("Formula", V::Map(formula))
}

/// A row whose value is given: `{"name", "value", "Category", "Tab",
/// ["parameters": {},] "Column", "read_type": "standard"}`.
fn val(name: &str, value: V, category: &str, tab: &str, column: &str, empty_parameters: bool) -> T {
    let mut row = T::new().s("name", name).with("value", value).s("Category", category).s("Tab", tab);
    if empty_parameters {
        row = row.with("parameters", no_bits());
    }
    row.s("Column", column).s("read_type", "standard")
}

/// The mode-register search parameters.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dp(
    offset_start: u64,
    value_to_find: u64,
    offset_base: u64,
    bit_start_dynamic: u32,
    bit_length_dynamic: i32,
    mchbar: u64,
    command: u64,
    offset2: u64,
) -> DynamicParams {
    DynamicParams {
        offset_start,
        value_to_find,
        offset_base,
        bit_start_dynamic,
        bit_length_dynamic,
        mchbar,
        command,
        offset2,
    }
}

/// A single dynamic-search row.
fn dyn_row(name: &str, category: &str, tab: &str, column: &str, params: DynamicParams, formula: Map) -> T {
    T::new()
        .s("name", name)
        .s("Category", category)
        .s("Tab", tab)
        .s("Column", column)
        .s("read_type", "dynamic")
        .with("dynamic_params", V::Dyn(params))
        .with("Formula", V::Map(formula))
}

/// A two-column dynamic-search row written with both sides spelled out.
#[allow(clippy::too_many_arguments)]
fn dual_dyn(
    name: &str,
    category: &str,
    tab: &str,
    column: &str,
    parameter_name: &str,
    names: (&str, &str),
    params_a: DynamicParams,
    formula: Map,
) -> T {
    let params_b = DynamicParams { mchbar: MCHBAR2, ..params_a };
    T::new()
        .s("name", name)
        .s("Category", category)
        .s("Tab", tab)
        .s("Column", column)
        .s("parameter_name", parameter_name)
        .s("name_a", names.0)
        .s("name_b", names.1)
        .s("read_type_a", "dynamic")
        .s("read_type_b", "dynamic")
        .with("dynamic_params_a", V::Dyn(params_a))
        .with("dynamic_params_b", V::Dyn(params_b))
        .with("Formula", V::Map(formula))
}

/// A Training mode-register row: both channels, CHA at MCHBAR and CHB at
/// MCHBAR2, as the literal spelled them.
fn training_mr(name: &str, category: &str, value: u64, bit_start: u32, bit_length: i32, command: u64, formula: Map) -> T {
    dual_dyn(
        name,
        category,
        "Training",
        "Left",
        "Name",
        ("CHA", "CHB"),
        dp(0xE600, value, 0xE200, bit_start, bit_length, MCHBAR, command, 0),
        formula,
    )
}

/// A Jedec-tab DFE row, channels A and B.
#[allow(clippy::too_many_arguments)]
fn jedec_dfe(
    name: &str,
    category: &str,
    column: &str,
    value: u64,
    bit_start: u32,
    bit_length: i32,
    offset2: u64,
    formula: Map,
) -> T {
    dual_dyn(
        name,
        category,
        "Jedec",
        column,
        "Channel",
        ("A", "B"),
        dp(0xE600, value, 0xE200, bit_start, bit_length, MCHBAR, 0, offset2),
        formula,
    )
}

/// One of the ODT DELAY rows: the same 0xE0xx field on both channels.
fn odt_delay(name: &str, offset: u64, bit_start: u32, bit_length: i32, channel_b: u64) -> T {
    T::new()
        .s("name", name)
        .with("address_a", V::Int(MCHBAR + offset))
        .with("address_b", V::Int(channel_b + offset))
        .s("Category", "ODT DELAY")
        .s("Tab", "Training")
        .with("parameters_a", bits(bit_start, bit_length))
        .with("parameters_b", bits(bit_start, bit_length))
        .s("Column", "Right")
        .s("read_type_a", "standard")
        .s("read_type_b", "standard")
}

/// A round-trip latency row of the RTL tab.
fn rtl(name: &str, address: u64, category: &str, bit_start: u32, column: &str) -> T {
    T::new()
        .s("name", name)
        .with("address", V::Int(address))
        .s("Category", category)
        .s("Tab", "RTL")
        .with("parameters", bits(bit_start, 8))
        .s("Column", column)
        .s("read_type", "standard")
}

/// The unnamed spacer between MC0 and MC1 on the RTL tab.
fn rtl_spacer(category: &str, column: &str) -> T {
    T::new()
        .s("name", "")
        .with("value", text(""))
        .s("Category", category)
        .s("Tab", "RTL")
        .with("parameters", bits(0, 8))
        .s("Column", column)
        .s("read_type", "standard")
}

/// The declared table.
pub(crate) fn declared(b: &Build) -> Vec<T> {
    let f = &b.f;
    let platform = b.platform;
    let mut rows = vec![
        val("CPU", text(get_cpu_name()), "General", "CPU", "Left", false),
        val("Motherboard", opt(get_motherboard_name()), "General", "CPU", "Left", false),
        val("Capacity", text(get_total_physical_memory(platform)), "General", "Timings", "Left", false),
        val("Speed", V::None, "General", "Timings", "Left", false),
        reg("Dram Ratio", 0x5E04, "General", "Timings", 0, 8, "Left"),
        reg_f("Multiplier", 0x5E04, "General", "Timings", 8, 4, "Left", f.multiplier.clone()),
        reg_f("Gear Mode", 0x5E04, "General", "Timings", 12, 2, "Left", f.gear_mode.clone()),
        val("CMD Stretch", text(get_cmd_stretch(None)), "General", "Timings", "Left", true),
        val("Channels", opt(detect_dual_channel_memory(platform)), "General", "Timings", "Left", true),
        val("IMC Frequency", text(get_imc_freq()), "Frequency", "CPU", "Left", true),
        val("BCLK", text(get_bclk_rd(platform)), "Frequency", "CPU", "Left", true),
        val("RING", text(get_ring_freq(platform)), "Frequency", "CPU", "Left", true),
        val("SA Voltage", text(get_sa()), "Voltage", "CPU", "Right", true),
        val("TX Voltage", text(get_tx()), "Voltage", "CPU", "Right", true),
        reg("tCL", 0xE070, "Primary", "Timings", 16, 7, "Left"),
        reg("tRCD", 0xE004, "Primary", "Timings", 19, 8, "Left"),
        reg("tRCDW", 0xE000, "Primary", "Timings", 24, 8, "Left"),
        reg("tRP", 0xE000, "Primary", "Timings", 0, 8, "Left"),
        reg("tRAS", 0xE004, "Primary", "Timings", 10, 9, "Left"),
        val("tRC", lazy(|| Some(get_trc_value(None))), "Primary", "Timings", "Left", true),
        val("CR", lazy(|| Some(get_command_rate(None))), "Primary", "Timings", "Left", true),
        reg("tRRD_S", 0xE008, "Secondary", "Timings", 9, 6, "Left"),
        reg("tRRD_L", 0xE008, "Secondary", "Timings", 15, 7, "Left"),
        dyn_row(
            "tWR",
            "Secondary",
            "Timings",
            "Left",
            dp(0xE600, 0x06, 0xE200, 0, 4, 0xFEDC0000, 0, 0),
            f.twr.clone(),
        ),
        reg("tRTP", 0xE000, "Secondary", "Timings", 13, 7, "Left"),
        reg("tFAW", 0xE008, "Secondary", "Timings", 0, 9, "Left"),
        val("tWTR_S", lazy(|| Some(get_twtr_s(None))), "Secondary", "Timings", "Left", true),
        val("tWTR_L", lazy(|| Some(get_twtr_l(None))), "Secondary", "Timings", "Left", true),
        reg("tCWL", 0xE070, "Secondary", "Timings", 24, 7, "Left"),
        dyn_row(
            "tCCDL",
            "Secondary",
            "Timings",
            "Left",
            dp(0xE600, 0x00, 0xE200, 0, 4, 0xFEDC0000, 0, 0),
            f.tccd_l.clone(),
        ),
        dyn_row(
            "tCCDL WR",
            "Secondary",
            "Timings",
            "Left",
            dp(0xE600, 0x00, 0xE200, 0, 4, 0xFEDC0000, 0, 0),
            f.tccd_l_wr.clone(),
        ),
        reg("tRDRD_sg", 0xE00C, "Tertiary", "Timings", 0, 7, "Right"),
        reg("tRDRD_dg", 0xE00C, "Tertiary", "Timings", 8, 7, "Right"),
        reg("tRDRD_dr", 0xE00C, "Tertiary", "Timings", 16, 7, "Right"),
        reg("tRDRD_dd", 0xE00C, "Tertiary", "Timings", 24, 7, "Right"),
        reg("tRDWR_sg", 0xE010, "Tertiary", "Timings", 0, 7, "Right"),
        reg("tRDWR_dg", 0xE010, "Tertiary", "Timings", 8, 7, "Right"),
        reg("tRDWR_dr", 0xE010, "Tertiary", "Timings", 16, 7, "Right"),
        reg("tRDWR_dd", 0xE010, "Tertiary", "Timings", 24, 7, "Right"),
        reg("tWRRD_sg", 0xE014, "Tertiary", "Timings", 0, 7, "Right"),
        reg("tWRRD_dg", 0xE014, "Tertiary", "Timings", 9, 7, "Right"),
        reg("tWRRD_dr", 0xE014, "Tertiary", "Timings", 18, 7, "Right"),
        reg("tWRRD_dd", 0xE014, "Tertiary", "Timings", 25, 7, "Right"),
        reg("tWRWR_sg", 0xE018, "Tertiary", "Timings", 0, 7, "Right"),
        reg("tWRWR_dg", 0xE018, "Tertiary", "Timings", 8, 7, "Right"),
        reg("tWRWR_dr", 0xE018, "Tertiary", "Timings", 16, 7, "Right"),
        reg("tWRWR_dd", 0xE018, "Tertiary", "Timings", 24, 7, "Right"),
        // Bit 7 of the register tRDRD_sg reads bits 0-6 of, so it sits with
        // the turnarounds rather than with a register of its own. Kept to the
        // foot of the section: it is a back-to-back allowance, not a delay,
        // and between tRDRD_sg and tRDRD_dg it split the group it belongs to.
        reg_f("Allow 2cyc B2B LPDDR", 0xE00C, "Tertiary", "Timings", 7, 1, "Right", f.disabled_enabled()),
        reg("tREFI", 0xE43C, "Refresh timings", "Timings", 0, 18, "Right"),
        reg("tREFIx9", 0xE438, "Refresh timings", "Timings", 24, 8, "Right"),
        reg("tRFC", 0xE43C, "Refresh timings", "Timings", 18, 13, "Right"),
        reg("tRFCpb", 0xE488, "Refresh timings", "Timings", 10, 11, "Right"),
        // The rest of 0xE488, which is the per-bank refresh register tRFCpb
        // already reads from: PBR is per-bank refresh, and its four controls
        // sit below tRFCpb's bits 10-20 with the ABR release above them.
        // All five match the reference tool's dump on this bench.
        reg_f("PBR Disable", 0xE488, "Refresh timings", "Timings", 0, 1, "Right", f.disabled_enabled()),
        reg_f("PBR OOO Disable", 0xE488, "Refresh timings", "Timings", 1, 1, "Right", f.disabled_enabled()),
        reg_f("PBR Disable On Hot", 0xE488, "Refresh timings", "Timings", 3, 1, "Right", f.disabled_enabled()),
        reg("PBR Exit On Idle", 0xE488, "Refresh timings", "Timings", 4, 6, "Right"),
        reg("Refresh ABR Release", 0xE488, "Refresh timings", "Timings", 21, 4, "Right"),
        reg("tZQCAL", 0xE44C, "Other Timings", "Timings", 0, 13, "Right"),
        reg("tZQCS", 0xE448, "Other Timings", "Timings", 10, 11, "Right"),
        reg("ZQCS period", 0xE448, "Other Timings", "Timings", 0, 10, "Right"),
        reg("tZQoper", 0xE440, "Other Timings", "Timings", 13, 11, "Right"),
        reg("tREFSBRD", 0xE00A, "Other Timings", "Timings", 8, 8, "Right"),
        reg("tMOD", 0xE440, "Other Timings", "Timings", 24, 8, "Right"),
        reg("tCAL", 0xE08C, "Other Timings", "Timings", 3, 3, "Right"),
        // The reference register map calls out the all-bank precharge extension
        // separately. Its embedded
        // Intel map places the four-bit field at 0xE000[10:7]; both controllers on
        // the Z790 DDR5 bench read zero, matching its dump.
        reg("TRPab_EXT", 0xE000, "Other Timings", "Timings", 7, 4, "Right"),
        reg("tWRPDEN", 0xE054, "Power down", "Timings", 0, 10, "Left"),
        reg("tRDPDEN", 0xE050, "Power down", "Timings", 21, 8, "Left"),
        reg("tPRPDEN", 0xE054, "Power down", "Timings", 27, 5, "Left"),
        reg("tAONPD", 0xE074, "Power down", "Timings", 0, 6, "Left"),
        reg("tCPDED", 0xE08C, "Power down", "Timings", 24, 5, "Left"),
        reg("tCKE", 0xE050, "Power down", "Timings", 0, 7, "Left"),
        reg("tXP", 0xE050, "Power down", "Timings", 7, 7, "Left"),
        reg("tWRPRE", 0xE004, "Power down", "Timings", 0, 10, "Left"),
        // The DRAM write-preamble state is decoded from its mode register on
        // Training. The controller register that used to occupy this row instead
        // contains per-channel Add/Dec tCWL training adjustments.
        reg("tXPDLL", 0xE050, "Power down", "Timings", 14, 7, "Left"),
        reg("tXSDLL", 0xE440, "Power down", "Timings", 0, 13, "Left"),
        // tXSR: located by setting it to 447 in BIOS and diffing full snapshots of
        // both controllers across the reboot. 0xE4C0 went 0x134 -> 0x1BF, i.e. 308
        // -> 447, on both controllers and in both sub-channel copies, with no other
        // bit in the register set either side. A control pair of snapshots taken
        // across a reboot that changed nothing the controller accepts shows this
        // register does not move on its own, unlike 0xE034/0xE040/0xE474/0xE4B0.
        reg("tXSR", 0xE4C0, "Power down", "Timings", 0, 13, "Left"),
        // tCKCKEH: located the same way, and the only field in the entire 32768
        // register sweep of both controllers to make the BIOS's own 20 -> 13
        // transition on both. The width is the open question rather than the
        // location: bits 12-14 read zero at both settings so the field could end
        // anywhere from bit 11, but bit 15 is set and unchanged either side, so 8
        // bits stops at the first boundary the data actually shows. Reading too
        // wide would surface a visibly wrong number if a neighbour ever fills;
        // reading too narrow would silently truncate a large value.
        reg("tCKCKEH", 0xE08C, "Power down", "Timings", 7, 8, "Left"),
        reg("tPPD", 0xE000, "Power down", "Timings", 20, 4, "Left"),
        reg("tCSH", 0xE054, "Power down", "Timings", 10, 6, "Left"),
        reg("tCSL", 0xE054, "Power down", "Timings", 16, 6, "Left"),
        reg("tCA2CS", 0xE054, "Power down", "Timings", 22, 5, "Left"),
        // The reference map's internal OREFRI field is presented to users as
        // "Rank Idle". This renames the existing reading rather than duplicating
        // it, so the UI uses the reference label for 0xE438[7:0].
        reg("Rank Idle", 0xE438, "Power down", "Timings", 0, 8, "Left"),
        // The rest of 0xE438. Rank Idle holds bits 0-7 and tREFIx9 bits 24-31,
        // and the reference tool's six names tile the gap between them exactly:
        // 8-11, 12-15, 16, 17, 18-19, 20-23. Read on the bench, all six agree
        // with its dump -- 6, 7, 0, 1, 2, 5 -- which is what pins the layout,
        // since a register that only half matched would not.
        reg("Refresh HP WM", 0xE438, "Refresh timings", "Timings", 8, 4, "Left"),
        reg("Refresh Panic WM", 0xE438, "Refresh timings", "Timings", 12, 4, "Left"),
        reg_f(
            "Count tREFI While Ref Disabled",
            0xE438,
            "Refresh timings",
            "Timings",
            16,
            1,
            "Left",
            f.disabled_enabled(),
        ),
        reg_f("HP Refresh On MRS", 0xE438, "Refresh timings", "Timings", 17, 1, "Left", f.disabled_enabled()),
        reg("Self Refresh Exit Debits", 0xE438, "Refresh timings", "Timings", 18, 2, "Left"),
        reg("Raise Block Wait", 0xE438, "Refresh timings", "Timings", 20, 4, "Left"),
        // The reference tools' POWERDOWN group carries six more than we did.
        //
        // tSR is written there as bit 52 of a 64-bit read at 0xE4C0. That is the
        // upper dword, so it is bits 20-25 of 0xE4C4 read as a plain field, the
        // same way tXSR above takes the lower half of the same register.
        reg("tSR", 0xE4C4, "Power down", "Timings", 20, 6, "Left"),
        reg("tOSCO", 0xE494, "Power down", "Timings", 0, 8, "Left"),
        reg("tPREMRR", 0xE494, "Power down", "Timings", 8, 7, "Left"),
        // 0xE494 also holds tMRRMRW at bits 15-21, reading 90. Not carried: it
        // was added here once standing in for a tPREMRW that neither reference
        // tool defines, and nothing has asked for it since.
        // Directly below tMRR in the same register, and reads 90 against the
        // reference tool's 90.
        reg("tMRRMRW", 0xE494, "Power down", "Timings", 15, 7, "Left"),
        reg("tMRR", 0xE494, "Power down", "Timings", 22, 7, "Left"),
        reg("tRFM", 0xE40C, "Power down", "Timings", 0, 11, "Left"),
        val(
            "tDLLK",
            lazy(|| get_dllk_timing(None).map(|cycles| cycles.to_string())),
            "Power down",
            "Timings",
            "Left",
            true,
        ),
        training_mr("RTT WR", "RTT", 0x22, 3, 3, 0, f.rtt_wr.clone()),
        training_mr("RTT NOM RD", "RTT", 0x23, 3, 3, 0, f.rtt_nom_rd.clone()),
        training_mr("RTT NOM WR", "RTT", 0x23, 0, 3, 0, f.rtt_nom_wr.clone()),
        training_mr("RTT PARK", "RTT", 0x08, 0, 3, 1, f.rtt_park.clone()),
        training_mr("RTT PARK DQS", "RTT", 0x06, 0, 3, 1, f.dqs_rtt_park.clone()),
        training_mr("RTT LOOPBACK", "RTT", 0x24, 0, 3, 0, f.rtt_loopback.clone()),
        training_mr("CA ODT GROUP A", "ODT", 0x05, 0, 3, 1, f.ca_odt.clone()),
        training_mr("CS ODT GROUP A", "ODT", 0x02, 0, 3, 1, f.cs_odt.clone()),
        training_mr("CK ODT GROUP A", "ODT", 0x01, 0, 3, 1, f.ck_odt.clone()),
        training_mr("CA ODT GROUP B", "ODT", 0x07, 0, 3, 1, f.ca_odt.clone()),
        training_mr("CS ODT GROUP B", "ODT", 0x04, 0, 3, 1, f.cs_odt.clone()),
        training_mr("CK ODT GROUP B", "ODT", 0x03, 0, 3, 1, f.ck_odt.clone()),
        training_mr("PULL UP", "RON", 0x05, 1, 2, 0, f.ron.clone()),
        training_mr("PULL DN", "RON", 0x05, 6, 2, 0, f.ron.clone()),
        reg("WrDS Up", 0x2CE8, "VREF", "Training", 0, 8, "Right"),
        reg("WrDS Dn", 0x2CE8, "VREF", "Training", 8, 8, "Right"),
        reg("RdODT Up", 0x2CE8, "VREF", "Training", 16, 8, "Right"),
        reg("RdODT Dn", 0x2CE8, "VREF", "Training", 24, 8, "Right"),
        reg("WrDSCmd Up", 0x2CEC, "VREF", "Training", 0, 8, "Right"),
        reg("WrDSCtl Up", 0x2CEC, "VREF", "Training", 8, 8, "Right"),
        reg("WrDSClk Up", 0x2CEC, "VREF", "Training", 16, 8, "Right"),
        reg("WrDSCke CS Up", 0x2CEC, "VREF", "Training", 24, 8, "Right"),
        // CKE/CS has an up level and no down level, so there is deliberately no
        // WrDSCke CS Dn row. One was added here -- as CKE CS VREFDN, before these
        // rows took the reference tools' names -- on the assumption that every
        // level comes in a pair split across 0x2CEC and 0x2CF0 on matching bits,
        // and it read bits 24-31 of 0x2CF0 by symmetry with the up register.
        // Three things say that byte is not a CKE/CS level: the reference map
        // stops after WrDSClk Dn and defines nothing at bit 24 of this register,
        // the string ckecs_vrefdn appears in neither tool's binary, and BIOS
        // exposes a CKE/CS up setting with no down one beside it. The byte is
        // real and reads 80; what it is remains unknown, which is why it is no
        // longer given a name that implies otherwise.
        //
        // The receiver reference and the QX comparator count. Neither is one of
        // the up/down pairs above: RX VREF is a 9-bit level rather than an 8-bit
        // one, and QXCOUNT is a count rather than a voltage at all, kept with the
        // VREF rows because it is what the comparator those levels feed reports.
        // Read 374 and 30 respectively on the bench.
        reg("RX VREF", 0x008C, "VREF", "Training", 14, 9, "Right"),
        reg("QXCOUNT", 0x3C94, "VREF", "Training", 18, 6, "Right"),
        reg("WrDSCmd Dn", 0x2CF0, "VREF", "Training", 0, 8, "Right"),
        reg("WrDSCtl Dn", 0x2CF0, "VREF", "Training", 8, 8, "Right"),
        reg("WrDSClk Dn", 0x2CF0, "VREF", "Training", 16, 8, "Right"),
    ];
    // DQ VREF is per device, not per channel: the table entry points at a
    // base and the four DRAM devices sit in the four bytes from there, so the
    // device is chosen by walking the base one byte at a time. Reading only
    // the first reported one device's level as though it were the module's.
    //
    // Confirmed on the bench: the entry at 0xE770 points at 0xE220, whose four
    // bytes are 69/66/69/68 and decode to 63.0 / 64.5 / 63.0 / 63.5 percent.
    for device in 0..4u64 {
        rows.push(dyn_row(
            &format!("DQ VREF D{device}"),
            "VREF Additional",
            "Training",
            "Right",
            dp(0xE600, 0x0A, 0xE200 + device, 0, 7, 0xFEDC0000, 0, 0),
            f.vref.clone(),
        ));
    }
    rows.extend([
        dyn_row(
            "CA VREF",
            "VREF Additional",
            "Training",
            "Right",
            dp(0xE600, 0x0B, 0xE200, 0, 7, 0xFEDC0000, 2, 0),
            f.vref.clone(),
        ),
        dyn_row(
            "CS VREF",
            "VREF Additional",
            "Training",
            "Right",
            dp(0xE600, 0x0C, 0xE200, 0, 7, 0xFEDC0000, 2, 0),
            f.vref.clone(),
        ),
        // 0x01BC tiles as CODEPI 0-5, CODEWL 6-11, BWSEL 12-17. CODEWL and
        // BWSEL match the reference tool exactly; CODEPI is a live DLL phase
        // code and was seen moving between 35 and 37 while sampled, so it is
        // read rather than compared against a captured number.
        reg("DLL Code PI", 0x01BC, "PHY Control", "Training", 0, 6, "Right"),
        reg("DLL Code WL", 0x01BC, "PHY Control", "Training", 6, 6, "Right"),
        reg("DLL BWSEL", 0x01BC, "PHY Control", "Training", 12, 6, "Right"),
        // The rest of the bandwidth-select and receive-enable group that sits with
        // DLL BWSEL. Verified on the bench at 4, 128 and 1253.
        reg("BWSEL Low Threshold", 0x3CA4, "PHY Control", "Training", 16, 6, "Right"),
        reg("DCC Control Code", 0x2C38, "PHY Control", "Training", 11, 8, "Right"),
        reg("Receive Enable PI", 0x00F4, "PHY Control", "Training", 0, 12, "Right"),
        reg_f("VTT ODT", 0x017C, "PHY Control", "Training", 0, 1, "Right", f.en_dis.clone()),
        reg_f("VSS ODT", 0x017C, "PHY Control", "Training", 1, 1, "Right", f.en_dis.clone()),
        reg_f("VDDQ ODT", 0x017C, "PHY Control", "Training", 2, 1, "Right", f.en_dis.clone()),
        // 0xE070 holds all four read/write duration and delay fields, one nibble
        // each. These rows used to carry only the two write fields and label the
        // two channel columns "RD" and "WR", which read as though the pair were
        // read and write -- they were the same bits on the two controllers, and
        // the actual read fields were not shown at all. The columns are the
        // channels here, as everywhere else on this tab.
        odt_delay("ODT Read Duration", 0xE070, 0, 4, b.channel_b),
        odt_delay("ODT Read Delay", 0xE070, 4, 4, b.channel_b),
        odt_delay("ODT Write Duration", 0xE070, 8, 4, b.channel_b),
        odt_delay("ODT Write Delay", 0xE070, 12, 4, b.channel_b),
        odt_delay("ODT FINETUNE", 0xE0B4, 0, 4, b.channel_b),
        odt_delay("ODT Write Early ODT", 0xE074, 6, 1, b.channel_b),
        dyn_row(
            "REFRESH",
            "REFRESH MODE",
            "Training",
            "Left",
            dp(0xE600, 0x04, 0xE200, 4, 1, 0xFEDC0000, 0, 0),
            f.refresh_mode.clone(),
        ),
        dyn_row(
            "ODTL WR ON",
            "ODTL",
            "Training",
            "Left",
            dp(0xE600, 0x25, 0xE200, 0, 3, 0xFEDC0000, 0, 0),
            f.odtl_on_wr.clone(),
        ),
        dyn_row(
            "ODTL WR OFF",
            "ODTL",
            "Training",
            "Left",
            dp(0xE600, 0x25, 0xE200, 3, 3, 0xFEDC0000, 0, 0),
            f.odtl_off_wr.clone(),
        ),
        dyn_row(
            "ODTL WR NT ON",
            "ODTL",
            "Training",
            "Left",
            dp(0xE600, 0x26, 0xE200, 0, 3, 0xFEDD0000, 0, 0),
            f.odtl_on_wr_nt.clone(),
        ),
        dyn_row(
            "ODTL WR NT OFF",
            "ODTL",
            "Training",
            "Left",
            dp(0xE600, 0x26, 0xE200, 3, 3, 0xFEDD0000, 0, 0),
            f.odtl_off_wr_nt.clone(),
        ),
        dyn_row(
            "ODTL RD NT ON",
            "ODTL",
            "Training",
            "Left",
            dp(0xE600, 0x27, 0xE200, 0, 3, 0xFEDD0000, 0, 0),
            f.odtl_on_rd_nt.clone(),
        ),
        dyn_row(
            "ODTL RD NT OFF",
            "ODTL",
            "Training",
            "Left",
            dp(0xE600, 0x27, 0xE200, 3, 3, 0xFEDD0000, 0, 0),
            f.odtl_off_rd_nt.clone(),
        ),
    ]);
    // The round-trip latencies, MC0 then MC1 under each channel heading, an
    // unnamed row between them.
    for (category, column, offsets) in
        [("Latency CHA", "Left", [0xE020u64, 0xE024]), ("Latency CHB", "Right", [0xE820, 0xE824])]
    {
        let channel = if category.ends_with("CHA") { "CHA" } else { "CHB" };
        for (controller, base) in [("MC0", MCHBAR), ("MC1", MCHBAR2)] {
            if controller == "MC1" {
                rows.push(rtl_spacer(category, column));
            }
            for rank in 0..8u32 {
                let offset = offsets[(rank / 4) as usize];
                rows.push(rtl(
                    &format!("RTL {controller} {channel} R{rank}"),
                    base + offset,
                    category,
                    (rank % 4) * 8,
                    column,
                ));
            }
        }
    }
    rows.extend([
        jedec_dfe("Global DFE Gain", "DFE", "Left", 0xF9, 0, 1, 2, f.dfe_enable.clone()),
        jedec_dfe("Global DFE Tap-1", "DFE", "Left", 0xF9, 1, 1, 2, f.dfe_enable.clone()),
        jedec_dfe("Global DFE Tap-3", "DFE", "Left", 0xF9, 3, 1, 2, f.dfe_enable.clone()),
        jedec_dfe("DFE GAIN Value", "DFE2", "Right", 0xF9, 0, 4, 1, f.dfe_gain.clone()),
        jedec_dfe("Global DFE Tap-2", "DFE2", "Right", 0xF9, 2, 1, 2, f.dfe_enable.clone()),
        jedec_dfe("Global DFE Tap-4", "DFE2", "Right", 0xF9, 4, 1, 2, f.dfe_enable.clone()),
        jedec_dfe("DFE Tap-1", "Tap 1", "Left", 0xF9, 7, 1, 0, f.dfe_tap_enable.clone()),
        jedec_dfe("DFE Tap-1 Value", "Tap 1", "Left", 0xF9, 0, 6, 0, f.dfe_tap1.clone()),
        jedec_dfe("DFE Tap-3", "Tap 3", "Left", 0xFB, 7, 1, 0, f.dfe_tap_enable.clone()),
        jedec_dfe("DFE Tap-3 Value", "Tap 3", "Left", 0xFB, 0, 6, 0, f.dfe_tap3.clone()),
        jedec_dfe("DFE Tap-2", "Tap 2", "Right", 0xFA, 7, 1, 0, f.dfe_tap_enable.clone()),
        jedec_dfe("DFE Tap-2 Value", "Tap 2", "Right", 0xFA, 0, 6, 0, f.dfe_tap2.clone()),
        jedec_dfe("DFE Tap-4", "Tap 4", "Right", 0xFC, 7, 1, 0, f.dfe_tap_enable.clone()),
        jedec_dfe("DFE Tap-4 Value", "Tap 4", "Right", 0xFC, 0, 6, 0, f.dfe_tap4.clone()),
    ]);
    rows
}
