//! A Summary laid out by hand: which rows it shows, in which panel and
//! column, in what order, under what labels.
//!
//! The layout names rows rather than indexing them, so it survives a restart
//! and a table that grew a row. It is saved per platform, since an AM5
//! table and an Intel one share few names. With no layout saved the Summary
//! is the built-in one, unchanged.

use serde_json::{Map, Value, json};

use rochviewer::row::Row;

use super::summary::{AboutPair, Line, Reading, SummaryPlan};

/// What one entry in a panel is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    /// A row's value, as the Summary reads it (a pair shows both modules).
    Value,
    /// A termination or drive row, read as the Training tab reads it.
    Signal,
    /// A two-module row, always as its pair.
    Dual,
    /// Two rows slashed together under one label.
    Pair,
    /// One two-module row as its A and B.
    Sides,
    /// A heading in the accent colour.
    Heading,
    /// An empty line (a hole, in the clocks block).
    Blank,
    /// Identity strip only: start a new line.
    Break,
}

impl ItemKind {
    fn key(self) -> &'static str {
        match self {
            ItemKind::Value => "value",
            ItemKind::Signal => "signal",
            ItemKind::Dual => "dual",
            ItemKind::Pair => "pair",
            ItemKind::Sides => "sides",
            ItemKind::Heading => "heading",
            ItemKind::Blank => "blank",
            ItemKind::Break => "break",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "value" => ItemKind::Value,
            "signal" => ItemKind::Signal,
            "dual" => ItemKind::Dual,
            "pair" => ItemKind::Pair,
            "sides" => ItemKind::Sides,
            "heading" => ItemKind::Heading,
            "blank" => ItemKind::Blank,
            "break" => ItemKind::Break,
            _ => return None,
        })
    }

    /// Whether the entry reads a row.
    pub fn reads_a_row(self) -> bool {
        matches!(self, ItemKind::Value | ItemKind::Signal | ItemKind::Dual | ItemKind::Pair | ItemKind::Sides)
    }
}

/// One entry: a row (by name, and by tab where two share a name), or a
/// heading, blank or line break.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub kind: ItemKind,
    pub name: String,
    pub tab: String,
    /// The second row of a pair.
    pub second: String,
    /// What the Summary shows beside the value, or the heading's text.
    pub label: String,
}

impl Item {
    #[cfg(test)]
    pub fn row(row: &Row) -> Item {
        Item { kind: ItemKind::Value, name: row.name.clone(), tab: row.tab.clone(), second: String::new(), label: row.label().to_string() }
    }

    pub fn heading(text: &str) -> Item {
        Item { kind: ItemKind::Heading, name: String::new(), tab: String::new(), second: String::new(), label: text.into() }
    }

    pub fn blank() -> Item {
        Item { kind: ItemKind::Blank, name: String::new(), tab: String::new(), second: String::new(), label: String::new() }
    }

    pub fn line_break() -> Item {
        Item { kind: ItemKind::Break, ..Item::blank() }
    }

    fn of(kind: ItemKind, rows: &[Row], index: usize, label: &str) -> Item {
        Item { kind, name: rows[index].name.clone(), tab: rows[index].tab.clone(), second: String::new(), label: label.into() }
    }

    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("kind".into(), Value::from(self.kind.key()));
        for (key, value) in [("name", &self.name), ("tab", &self.tab), ("second", &self.second), ("label", &self.label)] {
            if !value.is_empty() {
                map.insert(key.into(), Value::from(value.as_str()));
            }
        }
        Value::Object(map)
    }

    fn from_json(value: &Value) -> Option<Item> {
        let map = value.as_object()?;
        let text = |key: &str| map.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        Some(Item {
            kind: ItemKind::from_key(map.get("kind")?.as_str()?)?,
            name: text("name"),
            tab: text("tab"),
            second: text("second"),
            label: text("label"),
        })
    }
}

pub const MAX_COLUMNS: usize = 6;

/// A whole Summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryLayout {
    pub show_identity: bool,
    /// Packed along lines; a Break starts the next.
    pub identity: Vec<Item>,
    pub show_clocks: bool,
    /// Laid out on the columns, left to right then down; a Blank is a hole.
    pub clocks: Vec<Item>,
    pub columns: Vec<Vec<Item>>,
}

fn items_json(items: &[Item]) -> Value {
    Value::Array(items.iter().map(Item::to_json).collect())
}

fn items_from(value: Option<&Value>) -> Vec<Item> {
    value.and_then(Value::as_array).map(|items| items.iter().filter_map(Item::from_json).collect()).unwrap_or_default()
}

impl SummaryLayout {
    pub fn to_json(&self) -> Value {
        json!({
            "show_identity": self.show_identity,
            "identity": items_json(&self.identity),
            "show_clocks": self.show_clocks,
            "clocks": items_json(&self.clocks),
            "columns": Value::Array(self.columns.iter().map(|c| items_json(c)).collect()),
        })
    }

    pub fn from_json(value: &Value) -> Option<SummaryLayout> {
        let map = value.as_object()?;
        let mut columns: Vec<Vec<Item>> = map
            .get("columns")
            .and_then(Value::as_array)
            .map(|cols| cols.iter().map(|c| items_from(Some(c))).collect())
            .unwrap_or_default();
        columns.truncate(MAX_COLUMNS);
        if columns.is_empty() {
            columns.push(Vec::new());
        }
        Some(SummaryLayout {
            show_identity: map.get("show_identity").and_then(Value::as_bool).unwrap_or(true),
            identity: items_from(map.get("identity")),
            show_clocks: map.get("show_clocks").and_then(Value::as_bool).unwrap_or(true),
            clocks: items_from(map.get("clocks")),
            columns,
        })
    }

    /// The built-in Summary as a layout, so it can be edited from where it
    /// stands.
    pub fn from_plan(rows: &[Row], plan: &SummaryPlan) -> SummaryLayout {
        let mut identity = Vec::new();
        for (n, line) in plan.identity.iter().enumerate() {
            if n > 0 {
                identity.push(Item::line_break());
            }
            identity.extend(line.iter().map(|pair| Item::of(ItemKind::Value, rows, pair.index, &pair.label)));
        }
        let width = plan.columns.len().max(1);
        let mut clocks = Vec::new();
        for line in &plan.aligned {
            for c in 0..width {
                match line.get(c).cloned().flatten() {
                    Some(pair) => clocks.push(Item::of(ItemKind::Value, rows, pair.index, &pair.label)),
                    None => clocks.push(Item::blank()),
                }
            }
        }
        while clocks.last().is_some_and(|item| item.kind == ItemKind::Blank) {
            clocks.pop();
        }
        let columns = plan
            .columns
            .iter()
            .map(|lines| {
                lines
                    .iter()
                    .map(|line| match line {
                        Line::Row { index, label, reading } => {
                            let kind = match reading {
                                Reading::Compact => ItemKind::Value,
                                Reading::Signal => ItemKind::Signal,
                                Reading::Dual => ItemKind::Dual,
                            };
                            Item::of(kind, rows, *index, label)
                        }
                        Line::Rows { label, first, second, .. } => Item {
                            kind: ItemKind::Pair,
                            name: first.map(|i| rows[i].name.clone()).unwrap_or_default(),
                            tab: first.map(|i| rows[i].tab.clone()).unwrap_or_default(),
                            second: second.map(|i| rows[i].name.clone()).unwrap_or_default(),
                            label: label.clone(),
                        },
                        Line::Sides { label, index } => Item {
                            kind: ItemKind::Sides,
                            name: index.map(|i| rows[i].name.clone()).unwrap_or_default(),
                            tab: index.map(|i| rows[i].tab.clone()).unwrap_or_default(),
                            second: String::new(),
                            label: label.clone(),
                        },
                        Line::Heading { label } => Item::heading(label),
                        Line::Blank => Item::blank(),
                    })
                    .collect()
            })
            .collect();
        SummaryLayout { show_identity: true, identity, show_clocks: true, clocks, columns }
    }

    /// The plan the Summary draws from this layout. A row the table does not
    /// have is left out, so a layout made on one machine still opens on
    /// another.
    pub fn to_plan(&self, rows: &[Row]) -> SummaryPlan {
        let find = |item: &Item| find_row(rows, &item.name, &item.tab);
        let mut plan = SummaryPlan::default();
        if self.show_identity {
            let mut line: Vec<AboutPair> = Vec::new();
            for item in &self.identity {
                match item.kind {
                    ItemKind::Break => {
                        if !line.is_empty() {
                            plan.identity.push(std::mem::take(&mut line));
                        }
                    }
                    kind if kind.reads_a_row() => {
                        if let Some(index) = find(item) {
                            line.push(AboutPair { index, label: item.label.clone() });
                        }
                    }
                    _ => {}
                }
            }
            if !line.is_empty() {
                plan.identity.push(line);
            }
        }
        let width = self.columns.len().max(1);
        if self.show_clocks {
            let cells: Vec<Option<AboutPair>> = self
                .clocks
                .iter()
                .filter(|item| item.kind != ItemKind::Break)
                .map(|item| {
                    if item.kind.reads_a_row() {
                        find(item).map(|index| AboutPair { index, label: item.label.clone() })
                    } else {
                        None
                    }
                })
                .collect();
            for chunk in cells.chunks(width) {
                if chunk.iter().any(Option::is_some) {
                    plan.aligned.push(chunk.to_vec());
                }
            }
        }
        plan.columns = self
            .columns
            .iter()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| match item.kind {
                        ItemKind::Value | ItemKind::Signal | ItemKind::Dual => find(item).map(|index| Line::Row {
                            index,
                            label: item.label.clone(),
                            reading: match item.kind {
                                ItemKind::Signal => Reading::Signal,
                                ItemKind::Dual => Reading::Dual,
                                _ => Reading::Compact,
                            },
                        }),
                        ItemKind::Pair => {
                            let first = find(item);
                            let second = (!item.second.is_empty()).then(|| find_row(rows, &item.second, "")).flatten();
                            (first.is_some() || second.is_some()).then(|| Line::Rows {
                                label: item.label.clone(),
                                first,
                                second,
                                has_second: !item.second.is_empty(),
                            })
                        }
                        ItemKind::Sides => find(item).map(|index| Line::Sides { label: item.label.clone(), index: Some(index) }),
                        ItemKind::Heading => Some(Line::Heading { label: item.label.clone() }),
                        ItemKind::Blank => Some(Line::Blank),
                        ItemKind::Break => None,
                    })
                    .collect()
            })
            .collect();
        plan
    }

    /// Change the number of timing columns. A removed column's entries join
    /// the one before it rather than disappearing.
    pub fn set_column_count(&mut self, count: usize) {
        let count = count.clamp(1, MAX_COLUMNS);
        while self.columns.len() > count {
            let last = self.columns.pop().unwrap_or_default();
            if let Some(previous) = self.columns.last_mut() {
                previous.extend(last);
            }
        }
        while self.columns.len() < count {
            self.columns.push(Vec::new());
        }
    }
}

/// The row a layout entry names: the one on its tab when it says, else the
/// first of that name.
pub fn find_row(rows: &[Row], name: &str, tab: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    (!tab.is_empty())
        .then(|| rows.iter().position(|row| row.name == name && row.tab == tab))
        .flatten()
        .or_else(|| rows.iter().position(|row| row.name == name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rochviewer::platform_profiles::Platform;

    fn rows() -> Vec<Row> {
        let r = |name: &str, category: &str, tab: &str| Row::new(name, category, tab, "Left").with_value("1");
        vec![
            r("CPU", "Processor", "System Info"),
            r("Model", "Motherboard", "System Info"),
            r("BIOS", "Motherboard", "System Info"),
            r("DRAM Frequency", "Clocks", "System Info"),
            r("MCLK", "Clocks", "System Info"),
            r("tCL", "Primary", "Timings"),
            r("tRCD", "Primary", "Timings"),
            r("tREFI", "Refresh timings", "Timings"),
            r("Vcore snapshot", "CPU and motherboard", "Voltages").with_display_name("Vcore"),
            r("MCLK", "Clocks", "Sensors"),
        ]
    }

    #[test]
    fn the_built_in_summary_round_trips_through_a_layout() {
        let rows = rows();
        let plan = super::super::summary::summary_plan(&rows, Platform::Lga1700Ddr5);
        let layout = SummaryLayout::from_plan(&rows, &plan);
        assert_eq!(layout.to_plan(&rows), plan);
        let saved = layout.to_json();
        assert_eq!(SummaryLayout::from_json(&saved), Some(layout));
    }

    #[test]
    fn entries_find_their_own_tab_and_skip_what_is_missing() {
        let rows = rows();
        assert_eq!(find_row(&rows, "MCLK", "Sensors"), Some(9));
        assert_eq!(find_row(&rows, "MCLK", ""), Some(4));
        assert_eq!(find_row(&rows, "MCLK", "Nowhere"), Some(4));
        let layout = SummaryLayout {
            show_identity: true,
            identity: vec![Item::row(&rows[0]), Item::line_break(), Item::row(&rows[1])],
            show_clocks: false,
            clocks: vec![Item::row(&rows[3])],
            columns: vec![vec![Item::heading("Mine"), Item::row(&rows[5]), Item::blank(), Item { name: "gone".into(), ..Item::row(&rows[6]) }]],
        };
        let plan = layout.to_plan(&rows);
        assert_eq!(plan.identity.len(), 2);
        assert!(plan.aligned.is_empty(), "the clocks block is hidden");
        assert_eq!(plan.columns[0].len(), 3, "the missing row is left out");
        assert_eq!(plan.columns[0][0], Line::Heading { label: "Mine".into() });
        assert_eq!(plan.columns[0][2], Line::Blank);
    }

    #[test]
    fn clocks_flow_across_however_many_columns_there_are() {
        let rows = rows();
        let mut layout = SummaryLayout {
            show_identity: false,
            identity: Vec::new(),
            show_clocks: true,
            clocks: vec![Item::row(&rows[3]), Item::blank(), Item::row(&rows[4])],
            columns: vec![Vec::new(), Vec::new()],
        };
        let plan = layout.to_plan(&rows);
        assert_eq!(plan.aligned.len(), 2);
        assert!(plan.aligned[0][1].is_none());
        layout.set_column_count(3);
        assert_eq!(layout.to_plan(&rows).aligned.len(), 1);
    }

    #[test]
    fn fewer_columns_keep_every_entry() {
        let rows = rows();
        let mut layout = SummaryLayout {
            show_identity: true,
            identity: Vec::new(),
            show_clocks: true,
            clocks: Vec::new(),
            columns: vec![vec![Item::row(&rows[5])], vec![Item::row(&rows[6])], vec![Item::row(&rows[7])]],
        };
        layout.set_column_count(1);
        assert_eq!(layout.columns.len(), 1);
        assert_eq!(layout.columns[0].len(), 3);
        layout.set_column_count(9);
        assert_eq!(layout.columns.len(), MAX_COLUMNS);
    }
}
