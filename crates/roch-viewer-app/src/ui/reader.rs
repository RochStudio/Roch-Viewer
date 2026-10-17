//! Reading rows off the UI thread.
//!
//! Every value the window shows comes from a register, the SMU, an SMBus
//! device or WMI, and any of those can take milliseconds. The window never
//! waits for one: a worker thread reads every row once when the window opens,
//! re-reads the rows marked `live` on a one-second tick, and takes one more
//! pass at anything whose first read came back empty -- a fixed reading that
//! simply lost a race (UCLK:MCLK needs the clock block and MCLK to come back
//! in the same instant) genuinely does not change, so it is retried once
//! rather than made live.
//!
//! A row that reads two channels keeps both: the window decides, from the
//! module selector, whether to show one of them or the slashed pair, so a
//! change of selection redraws at once from what is already read.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rochviewer::display_values::{EM_DASH, WINDOWED_TABS, module_pair_text, read_side, summary_compact_ohm};
use rochviewer::row::{Channel, Row};
use rochviewer::timings::Backend;

/// How often rows marked `live` are re-read while the window is open. The
/// runtime caches a successful read for slightly less than this, so one tick
/// costs one read per transport rather than one per row.
pub const LIVE_REFRESH: Duration = Duration::from_millis(1000);
/// How long after startup the rows that came back empty are read once more.
/// Long enough for the opening burst of reads to be over, since contention
/// for the same mailbox is what blanked them.
pub const BLANK_RETRY: Duration = Duration::from_millis(1500);

enum Request {
    /// Read these rows again now (a tab re-read, a selection change).
    Reread(Vec<usize>),
    /// Show one channel of dual rows, or both.
    Select(Option<Channel>),
    Stop,
}

/// One row as last read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowValue {
    /// The row as the compact views show it for the reader's selection:
    /// Python's `_read_compact_value`.
    pub text: String,
    /// Each channel of a dual row, as read: `_read_compact_side(row, "a")`
    /// and `"b"`.
    pub sides: Option<(String, String)>,
    /// Readable but not in effect, so drawn dimmed. Asked here rather than
    /// on the UI thread: answering can take a register read.
    pub dimmed: bool,
}

/// Row values by index into `Backend::rows`.
#[derive(Clone, Default)]
pub struct Values {
    inner: Arc<Mutex<HashMap<usize, RowValue>>>,
}

impl Values {
    pub fn get(&self, index: usize) -> Option<String> {
        self.row(index).map(|value| value.text)
    }

    /// The value, or an em dash while it has not been read yet.
    pub fn text(&self, index: usize) -> String {
        self.get(index).unwrap_or_else(|| EM_DASH.to_string())
    }

    /// Everything known about one row.
    pub fn row(&self, index: usize) -> Option<RowValue> {
        self.inner.lock().ok()?.get(&index).cloned()
    }

    /// Each channel of a dual row.
    pub fn sides(&self, index: usize) -> Option<(String, String)> {
        self.row(index).and_then(|value| value.sides)
    }

    /// Whether a row is shown dimmed (false until read).
    pub fn dimmed(&self, index: usize) -> bool {
        self.row(index).is_some_and(|value| value.dimmed)
    }

    pub(crate) fn set(&self, index: usize, mut value: RowValue) -> bool {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // The Python chose a label's colour once, when it built the label,
        // and every later read changed its text only.
        if let Some(old) = map.get(&index) {
            value.dimmed = old.dimmed;
        }
        match map.get(&index) {
            Some(old) if *old == value => false,
            _ => {
                map.insert(index, value);
                true
            }
        }
    }
}

/// The compact text of a dual row from its two sides: one channel when one
/// is selected, otherwise the shared value or the A/B pair -- with a setting
/// only one channel reports standing alone when the row asks for that.
pub fn compact_dual_text(row: &Row, a: &str, b: &str, selected: Option<Channel>) -> String {
    match selected {
        Some(Channel::A) => return a.to_string(),
        Some(Channel::B) => return b.to_string(),
        None => {}
    }
    if row.lone_channel_alone {
        // A controller setting read on one channel only -- one DIMM, or both
        // on channel A -- is that channel's setting, not a pair with a
        // missing half: "Disabled", not "Disabled/-".
        let a_missing = a.trim() == EM_DASH;
        let b_missing = b.trim() == EM_DASH;
        if b_missing && !a_missing {
            return summary_compact_ohm(a);
        }
        if a_missing && !b_missing {
            return summary_compact_ohm(b);
        }
    }
    module_pair_text(a, b)
}

pub struct Reader {
    pub values: Values,
    sender: Sender<Request>,
}

impl Reader {
    /// Start the worker. `repaint` is called whenever a value changes.
    ///
    /// `first` is read before anything else -- what the opening page shows --
    /// so it fills in while the rest of the table is still being read.
    pub fn start(backend: Arc<Backend>, first: Vec<usize>, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let values = Values::default();
        let (sender, receiver) = mpsc::channel();
        let worker_values = values.clone();
        std::thread::Builder::new()
            .name("row-reader".into())
            .spawn(move || worker(backend, first, worker_values, receiver, repaint))
            .expect("spawning the reader thread");
        Reader { values, sender }
    }

    /// Re-take these rows now: each row's `refresh` is called first (once
    /// per distinct callable), which drops whatever cache it reads from.
    pub fn reread(&self, rows: Vec<usize>) {
        let _ = self.sender.send(Request::Reread(rows));
    }

    /// Narrow dual rows to one channel, or show both, and read them again.
    pub fn select_channel(&self, channel: Option<Channel>) {
        let _ = self.sender.send(Request::Select(channel));
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = self.sender.send(Request::Stop);
    }
}

/// Read one row as the window shows it. A panic in a reader is a blank, not
/// a dead worker.
pub fn read_row(backend: &Backend, row: &Row, channel: Option<Channel>) -> RowValue {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let dimmed = row.is_dimmed();
        if row.is_dual() {
            let a = read_side(row, Channel::A, backend.unavailable);
            let b = read_side(row, Channel::B, backend.unavailable);
            let text = compact_dual_text(row, &a, &b, channel);
            return RowValue { text, sides: Some((a, b)), dimmed };
        }
        RowValue { text: backend.read_value(row, channel), sides: None, dimmed }
    }))
    .unwrap_or_else(|_| RowValue { text: EM_DASH.to_string(), sides: None, dimmed: false })
}

/// Whether the main window draws this row at all. The Sensors rows are the
/// Telemetry window's, which reads them itself while it is open; the Python
/// main window never read them, and polling them here every second cost a
/// processor sample, the event log and the Super I/O for nothing.
pub fn shown_in_main_window(row: &Row) -> bool {
    !WINDOWED_TABS.contains(&row.tab.as_str())
}

/// How often values read in a long pass are handed to the window.
const PASS_REPAINT: Duration = Duration::from_millis(30);

fn worker(backend: Arc<Backend>, first: Vec<usize>, values: Values, receiver: Receiver<Request>, repaint: impl Fn() + Sync) {
    let mut channel: Option<Channel> = None;
    let read_all = |rows: &mut dyn Iterator<Item = usize>, channel: Option<Channel>| {
        let mut changed = false;
        let mut last_repaint = Instant::now();
        for index in rows {
            if changed && last_repaint.elapsed() >= PASS_REPAINT {
                repaint();
                changed = false;
                last_repaint = Instant::now();
            }
            let started = Instant::now();
            changed |= values.set(index, read_row(&backend, &backend.rows[index], channel));
            if super::timing::enabled() && started.elapsed().as_secs_f64() > 0.004 {
                super::timing::took(&format!("  slow row {} / {}", backend.rows[index].tab, backend.rows[index].name), started);
            }
        }
        changed
    };

    let first_pass = Instant::now();
    // `first` is what the opening page draws, and every row of it is read
    // whatever tab it belongs to: the Summary looks its clocks up by name,
    // the last row of a name wins as it did in the Python's dict, and for
    // MCLK and UCLK that is the Telemetry copy -- which the Python Summary
    // read, live, like any other label it drew.
    let shown: Vec<usize> =
        (0..backend.rows.len()).filter(|i| shown_in_main_window(&backend.rows[*i]) || first.contains(i)).collect();
    let mut order: Vec<usize> = first.into_iter().filter(|i| shown.contains(i)).collect();
    let mut queued = vec![false; backend.rows.len()];
    order.retain(|&i| !std::mem::replace(&mut queued[i], true));
    order.extend(shown.iter().copied().filter(|&i| !queued[i]));
    // The voltage snapshot is one long read -- every board rail through the
    // Super I/O and both PMICs over SMBus -- that the rest of the table does
    // not wait on. It runs beside the other rows rather than in front of
    // them; each transport takes its own lock, and the snapshot is computed
    // once whichever row asks first.
    let (slow, quick): (Vec<usize>, Vec<usize>) = order.into_iter().partition(|&i| backend.rows[i].tab == "Voltages");
    let changed = std::thread::scope(|scope| {
        let beside = scope.spawn(|| read_all(&mut slow.into_iter(), channel));
        let here = read_all(&mut quick.into_iter(), channel);
        here | beside.join().unwrap_or(true)
    });
    if changed {
        repaint();
    }
    super::timing::took("first read of every row", first_pass);
    let live: Vec<usize> = shown.iter().copied().filter(|&i| backend.rows[i].live).collect();
    let started = Instant::now();
    let mut retried_blanks = false;
    let mut next_live = Instant::now() + LIVE_REFRESH;

    loop {
        let now = Instant::now();
        let mut wake = next_live;
        if !retried_blanks {
            wake = wake.min(started + BLANK_RETRY);
        }
        match receiver.recv_timeout(wake.saturating_duration_since(now)) {
            Ok(Request::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Request::Reread(rows)) => {
                // Each distinct refresh once, as the Python deduplicated
                // them by identity: several rows share one cache.
                let mut called: Vec<*const ()> = Vec::new();
                for &index in &rows {
                    if let Some(refresh) = &backend.rows[index].refresh {
                        let key = Arc::as_ptr(refresh) as *const ();
                        if !called.contains(&key) {
                            called.push(key);
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| refresh()));
                        }
                    }
                }
                if read_all(&mut rows.into_iter(), channel) {
                    repaint();
                }
            }
            Ok(Request::Select(selected)) => {
                // Every label the window draws is read again, as the Python's
                // selection walked all of its module-aware labels -- the
                // single-value ones too, which picks up anything that has
                // changed or had come back blank.
                channel = selected;
                if read_all(&mut shown.iter().copied(), channel) {
                    repaint();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        if !retried_blanks && now >= started + BLANK_RETRY {
            // A row that reads blank again is left blank rather than
            // retried forever -- at that point the value really is
            // unavailable, and an em dash is the honest answer.
            retried_blanks = true;
            // Single-value rows only: the Python collected a blank for its
            // retry where it registered a label, and a two-channel row's
            // labels were never registered, so a blank pair stayed blank
            // until a module was picked.
            let blank: Vec<usize> = (0..backend.rows.len())
                .filter(|&i| {
                    !backend.rows[i].live && !backend.rows[i].is_dual() && values.get(i).is_some_and(|v| v.trim() == EM_DASH)
                })
                .collect();
            if read_all(&mut blank.into_iter(), channel) {
                repaint();
            }
        }
        if now >= next_live {
            if read_all(&mut live.iter().copied(), channel) {
                repaint();
            }
            next_live = Instant::now() + LIVE_REFRESH;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rochviewer::display_values::NOT_AVAILABLE;
    use rochviewer::platform_profiles::Platform;
    use rochviewer::row::{Side, Value};

    fn backend(rows: Vec<Row>) -> Backend {
        Backend { platform: Platform::Unsupported, rows, unavailable: NOT_AVAILABLE }
    }

    fn sides(a: &str, b: &str) -> Row {
        Row::new("tPHYRDL", "PHY", "Timings", "Left")
            .with_sides(Side::value(Value::text(a)), Side::value(Value::text(b)))
    }

    // --- tests/test_module_selector.py

    #[test]
    fn selection_switches_channel_and_all_restores_pairs() {
        let backend = backend(vec![sides("34", "36")]);
        let row = &backend.rows[0];
        assert_eq!(read_row(&backend, row, Some(Channel::B)).text, "36");
        assert_eq!(read_row(&backend, row, Some(Channel::A)).text, "34");
        let both = read_row(&backend, row, None);
        assert_eq!(both.text, "34/36");
        assert_eq!(both.sides, Some(("34".into(), "36".into())));
    }

    #[test]
    fn missing_channel_does_not_use_other_channel() {
        let backend = backend(vec![sides("34", "—")]);
        assert_eq!(read_row(&backend, &backend.rows[0], Some(Channel::B)).text, "—");
    }

    // --- tests/test_main_dual_rows.py: LoneChannelSummaryTest

    fn compact(a: &str, b: &str, lone: bool) -> String {
        let mut row = sides(a, b);
        row.name = "Nitro Mode".into();
        row.lone_channel_alone = lone;
        compact_dual_text(&row, a, b, None)
    }

    #[test]
    fn channel_a_alone_shows_its_value() {
        assert_eq!(compact("Disabled", "\u{2014}", true), "Disabled");
    }

    #[test]
    fn channel_b_alone_shows_its_value() {
        assert_eq!(compact("\u{2014}", "Enabled", true), "Enabled");
    }

    #[test]
    fn both_channels_stay_a_pair() {
        assert_eq!(compact("1", "2", true), "1/2");
    }

    #[test]
    fn an_unmarked_row_keeps_its_missing_half() {
        assert_eq!(compact("Disabled", "\u{2014}", false), "Disabled/\u{2014}");
    }

    // --- BlankRetryTest, as the worker answers it.

    #[test]
    fn a_blank_fixed_row_is_read_once_more_and_a_live_row_is_not_queued() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let fixed_reads = Arc::new(AtomicUsize::new(0));
        let counter = fixed_reads.clone();
        let fixed = Row::new("UCLK:MCLK", "Clocks", "System Info", "Left").with_lazy(move || {
            // Blank the first time, as a lost race does.
            if counter.fetch_add(1, Ordering::SeqCst) == 0 { None } else { Some("1:2".into()) }
        });
        let reader = Reader::start(Arc::new(Backend { platform: Platform::Unsupported, rows: vec![fixed], unavailable: EM_DASH }), Vec::new(), || {});
        let deadline = Instant::now() + Duration::from_secs(5);
        while reader.values.get(0).as_deref() != Some("1:2") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(reader.values.get(0).as_deref(), Some("1:2"));
        // Read twice: the first pass and the one retry, never on the tick.
        std::thread::sleep(LIVE_REFRESH + Duration::from_millis(200));
        assert_eq!(fixed_reads.load(Ordering::SeqCst), 2);
    }

    fn wait_for(reader: &Reader, index: usize, want: &str) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while reader.values.get(index).as_deref() != Some(want) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        reader.values.get(index)
    }

    #[test]
    fn a_telemetry_row_the_summary_draws_is_read_and_kept_live() {
        // The Summary's MCLK and UCLK are the Sensors copies (the last row
        // of the name), so leaving every Sensors row to the Telemetry window
        // left them as dashes. The rows the opening page asks for are read
        // whatever their tab, and a live one stays on the tick.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = reads.clone();
        let fixed = Row::new("MCLK", "Clocks", "System Info", "Left").with_value("4000 MHz");
        let mut sensor = Row::new("MCLK", "Clocks", "Sensors", "Left").with_lazy(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Some("4000 MHz".into())
        });
        sensor.live = true;
        let unasked = Row::new("Fan", "Fans", "Sensors", "Left").with_value("900 RPM");
        let reader = Reader::start(Arc::new(backend(vec![fixed, sensor, unasked])), vec![1], || {});
        assert_eq!(wait_for(&reader, 1, "4000 MHz").as_deref(), Some("4000 MHz"));
        std::thread::sleep(LIVE_REFRESH + Duration::from_millis(300));
        assert!(reads.load(Ordering::SeqCst) >= 2, "the live copy is re-read on the tick");
        // A Sensors row nothing on the page asked for is still left alone.
        assert_eq!(reader.values.get(2), None);
    }

    #[test]
    fn a_rows_colour_is_the_one_it_was_first_drawn_in() {
        let values = Values::default();
        values.set(0, RowValue { text: "416".into(), sides: None, dimmed: true });
        assert!(values.set(0, RowValue { text: "418".into(), sides: None, dimmed: false }));
        assert_eq!(values.get(0).as_deref(), Some("418"));
        assert!(values.dimmed(0), "the text changes, the colour does not");
    }

    #[test]
    fn picking_a_module_reads_the_single_value_rows_again_too() {
        // The Python re-read every module-aware label on a selection, not
        // only the two-channel ones.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = reads.clone();
        let single = Row::new("tREFI", "Refresh", "Timings", "Left").with_lazy(move || {
            Some(format!("{}", 24575 + counter.fetch_add(1, Ordering::SeqCst)))
        });
        let reader = Reader::start(Arc::new(backend(vec![single, sides("38", "38")])), Vec::new(), || {});
        assert_eq!(wait_for(&reader, 0, "24575").as_deref(), Some("24575"));
        reader.select_channel(Some(Channel::B));
        assert_eq!(wait_for(&reader, 0, "24576").as_deref(), Some("24576"));
    }

    #[test]
    fn a_blank_two_channel_row_waits_for_a_selection() {
        // The Python's blank retry covered the labels it registered, and a
        // two-channel row's never were: a pair that read blank stayed blank
        // until a module was picked.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = reads.clone();
        let side = move || if counter.fetch_add(1, Ordering::SeqCst) < 2 { None } else { Some("2N".to_string()) };
        let side_b = side.clone();
        let row = Row::new("CR", "Primary", "Timings", "Left")
            .with_sides(Side::value(Value::lazy(side)), Side::value(Value::lazy(side_b)));
        let reader = Reader::start(Arc::new(backend(vec![row])), Vec::new(), || {});
        std::thread::sleep(BLANK_RETRY + Duration::from_millis(400));
        assert_eq!(reads.load(Ordering::SeqCst), 2, "read once, not retried");
        reader.select_channel(Some(Channel::A));
        assert_eq!(wait_for(&reader, 0, "2N").as_deref(), Some("2N"));
    }

    #[test]
    fn a_reread_calls_each_shared_refresh_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let refreshed = Arc::new(AtomicUsize::new(0));
        let counter = refreshed.clone();
        let refresh: rochviewer::row::ActionFn = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let mut first = Row::new("a", "c", "Voltages", "Left").with_value("1");
        first.refresh = Some(refresh.clone());
        let mut second = Row::new("b", "c", "Voltages", "Left").with_value("2");
        second.refresh = Some(refresh);
        let reader = Reader::start(Arc::new(backend(vec![first, second])), Vec::new(), || {});
        reader.reread(vec![0, 1]);
        let deadline = Instant::now() + Duration::from_secs(5);
        while refreshed.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(refreshed.load(Ordering::SeqCst), 1);
    }
}
