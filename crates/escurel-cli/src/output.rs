//! Output rendering: JSON (the default, script/LLM contract) and a
//! generic human-readable table mode.
//!
//! The table renderer is deliberately *generic* — it walks whatever
//! JSON a command produced rather than carrying per-command layout, so
//! adding a command never means touching this file. An object's array-
//! of-objects fields render as tables (column union, first-seen order);
//! scalar fields render as `key: value` lines.

use serde_json::Value;

#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// Pretty-printed JSON (default). The stable, parseable contract.
    #[default]
    Json,
    /// Human-readable tables + key/value lines.
    Table,
}

pub fn emit(v: &Value, fmt: Format) -> anyhow::Result<()> {
    match fmt {
        Format::Json => println!("{}", serde_json::to_string_pretty(v)?),
        Format::Table => print!("{}", render(v)),
    }
    Ok(())
}

fn render(v: &Value) -> String {
    let mut out = String::new();
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                match val {
                    Value::Array(items)
                        if !items.is_empty() && items.iter().all(Value::is_object) =>
                    {
                        out.push_str(&format!("{k}:\n"));
                        out.push_str(&table(items));
                    }
                    Value::Array(items) => {
                        let joined = items.iter().map(scalar).collect::<Vec<_>>().join(", ");
                        out.push_str(&format!("{k}: {joined}\n"));
                    }
                    Value::Object(_) => {
                        out.push_str(&format!("{k}:\n"));
                        for line in render(val).lines() {
                            out.push_str(&format!("  {line}\n"));
                        }
                    }
                    _ => out.push_str(&format!("{k}: {}\n", scalar(val))),
                }
            }
        }
        _ => out.push_str(&format!("{}\n", scalar(v))),
    }
    out
}

/// Render an array of objects as a padded table.
fn table(rows: &[Value]) -> String {
    let mut cols: Vec<String> = Vec::new();
    for r in rows {
        if let Value::Object(m) = r {
            for k in m.keys() {
                if !cols.contains(k) {
                    cols.push(k.clone());
                }
            }
        }
    }
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            cols.iter()
                .map(|c| r.get(c).map(scalar).unwrap_or_default())
                .collect()
        })
        .collect();
    let mut widths: Vec<usize> = cols.iter().map(String::len).collect();
    for row in &cells {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(c.len());
        }
    }
    let pad = |row: &[String]| -> String {
        row.iter()
            .enumerate()
            .map(|(i, c)| format!("{c:width$}", width = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let mut s = String::new();
    s.push_str(&format!("  {}\n", pad(&cols)));
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    s.push_str(&format!("  {}\n", pad(&sep)));
    for row in &cells {
        s.push_str(&format!("  {}\n", pad(row)));
    }
    s
}

/// One JSON value as a single-line cell.
fn scalar(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}
