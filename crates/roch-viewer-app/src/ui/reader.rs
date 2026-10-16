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

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rochviewer::display_values::EM_DASH;
use rochviewer::row::Channel;
use rochviewer::timings::Backend;

/// How often live rows are read again.
pub const LIVE_REFRESH: Duration = Duration::from_millis(1000);
/// When the one retry of blank fixed rows happens.
pub const BLANK_RETRY: Duration = Duration::from_millis(1500);

enum Request {
    /// Read these rows again now (a tab re-read, a selection change).
    Reread(Vec<usize>),
    /// Show one channel of dual rows, or both.
    Select(Option<Channel>),
    Stop,
}

/// Row values by index into `Backend::rows`.
#[derive(Clone, Default)]
pub struct Values {
    inner: Arc<Mutex<HashMap<usize, String>>>,
}

impl Values {
    pub fn get(&self, index: usize) -> Option<String> {
        self.inner.lock().ok()?.get(&index).cloned()
    }

    /// The value, or an em dash while it has not been read yet.
    pub fn text(&self, index: usize) -> String {
        self.get(index).unwrap_or_else(|| EM_DASH.to_string())
    }

    fn set(&self, index: usize, text: String) -> bool {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(&index) {
            Some(old) if *old == text => false,
            _ => {
                map.insert(index, text);
                true
            }
        }
    }
}

pub struct Reader {
    pub values: Values,
    sender: Sender<Request>,
}

impl Reader {
    /// Start the worker. `repaint` is called whenever a value changes.
    pub fn start(backend: Arc<Backend>, repaint: impl Fn() + Send + 'static) -> Self {
        let values = Values::default();
        let (sender, receiver) = mpsc::channel();
        let worker_values = values.clone();
        std::thread::Builder::new()
            .name("row-reader".into())
            .spawn(move || worker(backend, worker_values, receiver, repaint))
            .expect("spawning the reader thread");
        Reader { values, sender }
    }

    pub fn reread(&self, rows: Vec<usize>) {
        let _ = self.sender.send(Request::Reread(rows));
    }

    pub fn select_channel(&self, channel: Option<Channel>) {
        let _ = self.sender.send(Request::Select(channel));
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = self.sender.send(Request::Stop);
    }
}

fn read_one(backend: &Backend, index: usize, channel: Option<Channel>) -> String {
    let row = &backend.rows[index];
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| backend.read_value(row, channel)))
        .unwrap_or_else(|_| EM_DASH.to_string())
}

fn worker(
    backend: Arc<Backend>,
    values: Values,
    receiver: Receiver<Request>,
    repaint: impl Fn(),
) {
    let mut channel: Option<Channel> = None;
    let read_all = |rows: &mut dyn Iterator<Item = usize>, channel: Option<Channel>| {
        let mut changed = false;
        for index in rows {
            changed |= values.set(index, read_one(&backend, index, channel));
        }
        changed
    };

    if read_all(&mut (0..backend.rows.len()), channel) {
        repaint();
    }
    let live: Vec<usize> = (0..backend.rows.len()).filter(|&i| backend.rows[i].live).collect();
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
                for &index in &rows {
                    if let Some(refresh) = &backend.rows[index].refresh {
                        refresh();
                    }
                }
                if read_all(&mut rows.into_iter(), channel) {
                    repaint();
                }
            }
            Ok(Request::Select(selected)) => {
                channel = selected;
                let dual: Vec<usize> = (0..backend.rows.len()).filter(|&i| backend.rows[i].is_dual()).collect();
                if read_all(&mut dual.into_iter(), channel) {
                    repaint();
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        if !retried_blanks && now >= started + BLANK_RETRY {
            retried_blanks = true;
            let blank: Vec<usize> = (0..backend.rows.len())
                .filter(|&i| !backend.rows[i].live && values.get(i).is_some_and(|v| v.trim() == EM_DASH))
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
