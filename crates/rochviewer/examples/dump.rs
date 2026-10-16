//! Print every row of the active platform's table as the window shows it,
//! as JSON, for comparing one build's readings against another's.
//!
//! Run elevated from the project root, with the driver beside it:
//!
//! ```text
//! cargo run -p rochviewer --example dump -- out.json
//! ```

use rochviewer::timings::load_active_backend;
use serde_json::json;

fn main() {
    let backend = load_active_backend();
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
    let text = serde_json::to_string_pretty(&out).expect("serialising the dump");
    match std::env::args().nth(1) {
        Some(path) => std::fs::write(&path, text).expect("writing the dump"),
        None => println!("{text}"),
    }
    eprintln!("{}: {} rows", backend.platform.id(), backend.rows.len());
}
