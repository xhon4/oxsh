use crate::value::{CmpOp, Value};
use std::collections::HashSet;

/// All structured pipeline command names
pub static STRUCTURED_COMMANDS: &[&str] = &[
    "where",
    "select",
    "sort-by",
    "reverse",
    "first",
    "last",
    "count",
    "uniq",
    "flatten",
    "from-json",
    "to-json",
    "to-table",
    "get",
];

pub fn is_structured_command(name: &str) -> bool {
    STRUCTURED_COMMANDS.contains(&name)
}

/// Execute a structured command with given input data.
/// Returns (output, exit_code, output_is_structured).
pub fn run_structured(cmd: &str, args: &[String], input: &str) -> (String, i32, bool) {
    match cmd {
        "from-json" => cmd_from_json(input),
        "to-json" => cmd_to_json(args, input),
        "to-table" => cmd_to_table(input),
        "where" => cmd_where(args, input),
        "select" => cmd_select(args, input),
        "sort-by" => cmd_sort_by(args, input),
        "reverse" => cmd_reverse(input),
        "first" => cmd_first(args, input),
        "last" => cmd_last(args, input),
        "count" => cmd_count(input),
        "uniq" => cmd_uniq(input),
        "get" => cmd_get(args, input),
        "flatten" => cmd_flatten(input),
        _ => (format!("oxsh: unknown structured command: {cmd}\n"), 1, false),
    }
}

/// Parse input: try JSON first, fall back to list of lines.
/// Warns when the input looks like JSON but fails to parse.
fn parse_input(input: &str) -> Value {
    let trimmed = input.trim();
    if let Ok(val) = Value::from_json(trimmed) {
        return val;
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        eprintln!("oxsh: warning: input looks like JSON but could not be parsed — treating as text lines");
    }
    Value::List(
        input
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| Value::String(l.to_string()))
            .collect(),
    )
}

fn cmd_from_json(input: &str) -> (String, i32, bool) {
    match Value::from_json(input.trim()) {
        Ok(val) => (val.to_json() + "\n", 0, true),
        Err(e) => (format!("from-json: {e}\n"), 1, false),
    }
}

fn cmd_to_json(args: &[String], input: &str) -> (String, i32, bool) {
    let val = parse_input(input);
    let pretty = args.iter().any(|a| a == "--pretty" || a == "-p");
    let out = if pretty {
        val.to_json_pretty()
    } else {
        val.to_json()
    };
    (out + "\n", 0, false)
}

fn cmd_to_table(input: &str) -> (String, i32, bool) {
    let val = parse_input(input);
    (val.format_table(), 0, false)
}

// where FIELD OP VALUE
// e.g.: where cpu > 10
//       where .status.phase == Running
fn cmd_where(args: &[String], input: &str) -> (String, i32, bool) {
    if args.len() < 3 {
        return (
            "usage: where FIELD OP VALUE\n  operators: == != > < >= <= =~ ^=\n".into(),
            1,
            false,
        );
    }

    let field = args[0].trim_start_matches('.');
    let op = match CmpOp::parse(&args[1]) {
        Some(op) => op,
        None => return (format!("where: unknown operator '{}'\n", args[1]), 1, false),
    };
    let right = &args[2];

    let val = parse_input(input);
    match val {
        Value::List(items) => {
            let filtered: Vec<Value> = items
                .into_iter()
                .filter(|item| {
                    if let Some(field_val) = item.get_field(field) {
                        op.compare(field_val, right)
                    } else {
                        false
                    }
                })
                .collect();
            (Value::List(filtered).to_json() + "\n", 0, true)
        }
        other => {
            if let Some(field_val) = other.get_field(field) {
                if op.compare(field_val, right) {
                    (other.to_json() + "\n", 0, true)
                } else {
                    (Value::List(vec![]).to_json() + "\n", 0, true)
                }
            } else {
                (Value::List(vec![]).to_json() + "\n", 0, true)
            }
        }
    }
}

// select FIELD1 FIELD2 ...
fn cmd_select(args: &[String], input: &str) -> (String, i32, bool) {
    if args.is_empty() {
        return ("usage: select FIELD1 FIELD2 ...\n".into(), 1, false);
    }

    let fields: Vec<&str> = args.iter().map(|a| a.trim_start_matches('.')).collect();
    let val = parse_input(input);

    match val {
        Value::List(items) => {
            let projected: Vec<Value> = items
                .iter()
                .map(|item| {
                    let mut record = indexmap::IndexMap::new();
                    for &field in &fields {
                        if let Some(v) = item.get_field(field) {
                            record.insert(field.to_string(), v.clone());
                        }
                    }
                    Value::Record(record)
                })
                .collect();
            (Value::List(projected).to_json() + "\n", 0, true)
        }
        Value::Record(map) => {
            let mut record = indexmap::IndexMap::new();
            for &field in &fields {
                if let Some(v) = map.get(field) {
                    record.insert(field.to_string(), v.clone());
                }
            }
            (Value::Record(record).to_json() + "\n", 0, true)
        }
        _ => (val.to_json() + "\n", 0, true),
    }
}

// sort-by FIELD [--desc]
fn cmd_sort_by(args: &[String], input: &str) -> (String, i32, bool) {
    if args.is_empty() {
        return ("usage: sort-by FIELD [--desc]\n".into(), 1, false);
    }

    let field = args[0].trim_start_matches('.');
    let desc = args.iter().any(|a| a == "--desc" || a == "-d");
    let val = parse_input(input);

    match val {
        Value::List(mut items) => {
            items.sort_by(|a, b| {
                let va = a.get_field(field);
                let vb = b.get_field(field);
                let cmp = match (
                    va.and_then(|v| v.as_number()),
                    vb.and_then(|v| v.as_number()),
                ) {
                    (Some(na), Some(nb)) => {
                        na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    _ => {
                        let sa = va.map(|v| v.as_str_lossy()).unwrap_or_default();
                        let sb = vb.map(|v| v.as_str_lossy()).unwrap_or_default();
                        sa.cmp(&sb)
                    }
                };
                if desc {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
            (Value::List(items).to_json() + "\n", 0, true)
        }
        _ => (val.to_json() + "\n", 0, true),
    }
}

fn cmd_reverse(input: &str) -> (String, i32, bool) {
    let val = parse_input(input);
    match val {
        Value::List(mut items) => {
            items.reverse();
            (Value::List(items).to_json() + "\n", 0, true)
        }
        _ => (val.to_json() + "\n", 0, true),
    }
}

fn cmd_first(args: &[String], input: &str) -> (String, i32, bool) {
    let n: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(1);
    let val = parse_input(input);
    match val {
        Value::List(items) => {
            let taken: Vec<Value> = items.into_iter().take(n).collect();
            (Value::List(taken).to_json() + "\n", 0, true)
        }
        _ => (val.to_json() + "\n", 0, true),
    }
}

fn cmd_last(args: &[String], input: &str) -> (String, i32, bool) {
    let n: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(1);
    let val = parse_input(input);
    match val {
        Value::List(items) => {
            let skip = items.len().saturating_sub(n);
            let taken: Vec<Value> = items.into_iter().skip(skip).collect();
            (Value::List(taken).to_json() + "\n", 0, true)
        }
        _ => (val.to_json() + "\n", 0, true),
    }
}

fn cmd_count(input: &str) -> (String, i32, bool) {
    let val = parse_input(input);
    match val {
        Value::List(items) => (format!("{}\n", items.len()), 0, false),
        _ => ("1\n".into(), 0, false),
    }
}

fn cmd_uniq(input: &str) -> (String, i32, bool) {
    let val = parse_input(input);
    match val {
        Value::List(items) => {
            let mut seen = HashSet::new();
            let mut unique = Vec::new();
            for item in items {
                let key = item.to_json();
                if seen.insert(key) {
                    unique.push(item);
                }
            }
            (Value::List(unique).to_json() + "\n", 0, true)
        }
        _ => (val.to_json() + "\n", 0, true),
    }
}

// get FIELD — extract a single field from records
fn cmd_get(args: &[String], input: &str) -> (String, i32, bool) {
    if args.is_empty() {
        return ("usage: get FIELD\n".into(), 1, false);
    }
    let field = args[0].trim_start_matches('.');
    let val = parse_input(input);

    match val {
        Value::List(items) => {
            let extracted: Vec<Value> = items
                .iter()
                .filter_map(|item| item.get_field(field).cloned())
                .collect();
            (Value::List(extracted).to_json() + "\n", 0, true)
        }
        other => {
            if let Some(v) = other.get_field(field) {
                (v.to_json() + "\n", 0, true)
            } else {
                ("null\n".into(), 0, true)
            }
        }
    }
}

fn cmd_flatten(input: &str) -> (String, i32, bool) {
    let val = parse_input(input);
    match val {
        Value::List(items) => {
            let mut flat = Vec::new();
            for item in items {
                if let Value::List(sub) = item {
                    flat.extend(sub);
                } else {
                    flat.push(item);
                }
            }
            (Value::List(flat).to_json() + "\n", 0, true)
        }
        _ => (val.to_json() + "\n", 0, true),
    }
}
