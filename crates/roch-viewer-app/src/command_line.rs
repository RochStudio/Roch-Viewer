//! What the program does besides open its window, on Linux.
//!
//! Windows has one thing to do: open the window, elevated. Linux has the helper
//! the window starts, which is this same executable, and a machine with no desktop
//! or a bug report may still want the readings as text.

use rochviewer::hardware::linux::{HELPER_ARG, HELPER_FILE_NAME, helper};

const USAGE: &str = "\
Roch Viewer: a read-only memory-controller and timing viewer.

  rochviewer                 open the window
  rochviewer dump [FILE]     every row as JSON, to FILE or the terminal
  rochviewer setup           (as root) stop asking for access at each launch
  rochviewer setup --status  whether that is set up
  rochviewer setup --undo    (as root) remove it
  rochviewer --version
";

/// Handle the command line. `Some(code)` when there is nothing left to do.
pub fn run() -> Option<i32> {
    // Installed under this name, carrying capabilities, it is only ever the helper:
    // the rest of the program has no business running with them.
    let exe = std::env::current_exe().ok();
    if exe.as_deref().and_then(|exe| exe.file_name()).is_some_and(|name| name == HELPER_FILE_NAME) {
        return Some(helper::serve());
    }
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let first = arguments.first()?.as_str();
    Some(match first {
        HELPER_ARG => helper::serve(),
        "dump" => dump(arguments.get(1).map(String::as_str)),
        "setup" => crate::linux_setup::run(&arguments[1..]),
        "--version" | "-V" => {
            println!("{} {}", rochviewer::version::APP_NAME, rochviewer::version::VERSION);
            0
        }
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            0
        }
        other => {
            eprint!("Unknown argument: {other}\n\n{USAGE}");
            2
        }
    })
}

/// Every row of the active platform's table as the window shows it, as JSON.
fn dump(path: Option<&str>) -> i32 {
    use serde_json::json;
    if let Some(problem) = rochviewer::hardware::driver_error() {
        eprintln!("{problem}");
    }
    let backend = rochviewer::timings::load_active_backend();
    // Everything once, with the answers thrown away: clocks and power are rates
    // against an earlier reading, and read only once they would all come back blank.
    for row in &backend.rows {
        let _ = backend.read_value(row, None);
    }
    std::thread::sleep(std::time::Duration::from_millis(400));
    let rows: Vec<_> = backend
        .rows
        .iter()
        .map(|row| {
            json!({
                "tab": row.tab,
                "category": row.category,
                "column": row.column,
                "name": row.name,
                "label": row.label(),
                "dual": row.is_dual(),
                "live": row.live,
                "value": backend.read_value(row, None),
            })
        })
        .collect();
    let out = json!({ "platform": backend.platform.id(), "rows": rows });
    let text = match serde_json::to_string_pretty(&out) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("Could not write the rows out: {error}");
            return 1;
        }
    };
    let written = match path {
        Some(path) => std::fs::write(path, text),
        None => {
            println!("{text}");
            Ok(())
        }
    };
    if let Err(error) = written {
        eprintln!("Could not write {}: {error}", path.unwrap_or("the terminal"));
        return 1;
    }
    eprintln!("{}: {} rows", backend.platform.id(), backend.rows.len());
    0
}

