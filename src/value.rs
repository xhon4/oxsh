use indexmap::IndexMap;
use std::fmt;

/// Structured data type for oxsh's hybrid object pipeline.
/// Commands can emit Values (JSON-like) instead of raw text.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nothing,
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    List(Vec<Value>),
    Record(IndexMap<String, Value>),
}

impl Value {
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        let json: serde_json::Value = serde_json::from_str(s)?;
        Ok(Self::from_json_value(json))
    }

    fn from_json_value(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Value::Nothing,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else {
                    Value::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            serde_json::Value::String(s) => Value::String(s),
            serde_json::Value::Array(arr) => {
                Value::List(arr.into_iter().map(Self::from_json_value).collect())
            }
            serde_json::Value::Object(map) => {
                Value::Record(
                    map.into_iter()
                        .map(|(k, v)| (k, Self::from_json_value(v)))
                        .collect(),
                )
            }
        }
    }

    fn to_json_value(&self) -> serde_json::Value {
        match self {
            Value::Nothing => serde_json::Value::Null,
            Value::String(s) => serde_json::Value::String(s.clone()),
            Value::Int(i) => serde_json::json!(*i),
            Value::Float(f) => serde_json::json!(*f),
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::List(list) => {
                serde_json::Value::Array(list.iter().map(|v| v.to_json_value()).collect())
            }
            Value::Record(map) => {
                let obj: serde_json::Map<String, serde_json::Value> = map
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json_value()))
                    .collect();
                serde_json::Value::Object(obj)
            }
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.to_json_value()).unwrap_or_default()
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.to_json_value()).unwrap_or_default()
    }

    /// Get a nested field via dot notation: "status.phase"
    pub fn get_field(&self, path: &str) -> Option<&Value> {
        let parts: Vec<&str> = path.split('.').collect();
        let mut current = self;
        for part in parts {
            match current {
                Value::Record(map) => {
                    current = map.get(part)?;
                }
                Value::List(items) => {
                    if let Ok(idx) = part.parse::<usize>() {
                        current = items.get(idx)?;
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        Some(current)
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::String(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

    pub fn as_str_lossy(&self) -> String {
        match self {
            Value::Nothing => String::new(),
            Value::String(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => self.to_json(),
        }
    }

    /// Render as a formatted table for terminal output
    pub fn format_table(&self) -> String {
        match self {
            Value::List(items) if !items.is_empty() => {
                // Collect column names from all records
                let mut columns: Vec<String> = Vec::new();
                for item in items {
                    if let Value::Record(map) = item {
                        for key in map.keys() {
                            if !columns.contains(key) {
                                columns.push(key.clone());
                            }
                        }
                    }
                }

                if columns.is_empty() {
                    // List of non-records: one item per line
                    return items
                        .iter()
                        .map(|v| v.as_str_lossy())
                        .collect::<Vec<_>>()
                        .join("\n")
                        + "\n";
                }

                // Calculate column widths
                let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
                let rows: Vec<Vec<String>> = items
                    .iter()
                    .map(|item| {
                        columns
                            .iter()
                            .enumerate()
                            .map(|(ci, col)| {
                                let val = if let Value::Record(map) = item {
                                    map.get(col)
                                        .map(|v| v.as_str_lossy())
                                        .unwrap_or_default()
                                } else {
                                    String::new()
                                };
                                widths[ci] = widths[ci].max(val.len());
                                val
                            })
                            .collect()
                    })
                    .collect();

                let mut out = String::new();

                // Header
                let header: Vec<String> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
                    .collect();
                out.push_str(&header.join("  "));
                out.push('\n');

                // Separator
                let sep: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
                out.push_str(&sep.join("──"));
                out.push('\n');

                // Rows
                for row in &rows {
                    let cells: Vec<String> = row
                        .iter()
                        .enumerate()
                        .map(|(i, v)| format!("{:<width$}", v, width = widths[i]))
                        .collect();
                    out.push_str(&cells.join("  "));
                    out.push('\n');
                }
                out
            }
            Value::Record(map) => {
                let max_key = map.keys().map(|k| k.len()).max().unwrap_or(0);
                let mut out = String::new();
                for (k, v) in map {
                    out.push_str(&format!(
                        "{:<width$}  {}\n",
                        k,
                        v.as_str_lossy(),
                        width = max_key
                    ));
                }
                out
            }
            _ => format!("{}\n", self.as_str_lossy()),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str_lossy())
    }
}

// --- Comparison operators for structured commands ---

#[derive(Debug)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    Contains,
    StartsWith,
}

impl CmpOp {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "==" | "=" | "eq" => Some(CmpOp::Eq),
            "!=" | "ne" => Some(CmpOp::Ne),
            ">" | "gt" => Some(CmpOp::Gt),
            "<" | "lt" => Some(CmpOp::Lt),
            ">=" | "ge" | "gte" => Some(CmpOp::Ge),
            "<=" | "le" | "lte" => Some(CmpOp::Le),
            "=~" | "contains" => Some(CmpOp::Contains),
            "^=" | "starts-with" => Some(CmpOp::StartsWith),
            _ => None,
        }
    }

    pub fn compare(&self, left: &Value, right_str: &str) -> bool {
        match self {
            CmpOp::Eq => {
                if let (Some(ln), Ok(rn)) = (left.as_number(), right_str.parse::<f64>()) {
                    return (ln - rn).abs() < f64::EPSILON;
                }
                left.as_str_lossy() == right_str
            }
            CmpOp::Ne => {
                if let (Some(ln), Ok(rn)) = (left.as_number(), right_str.parse::<f64>()) {
                    return (ln - rn).abs() >= f64::EPSILON;
                }
                left.as_str_lossy() != right_str
            }
            CmpOp::Gt => {
                if let (Some(ln), Ok(rn)) = (left.as_number(), right_str.parse::<f64>()) {
                    return ln > rn;
                }
                left.as_str_lossy().as_str() > right_str
            }
            CmpOp::Lt => {
                if let (Some(ln), Ok(rn)) = (left.as_number(), right_str.parse::<f64>()) {
                    return ln < rn;
                }
                left.as_str_lossy().as_str() < right_str
            }
            CmpOp::Ge => {
                if let (Some(ln), Ok(rn)) = (left.as_number(), right_str.parse::<f64>()) {
                    return ln >= rn;
                }
                left.as_str_lossy().as_str() >= right_str
            }
            CmpOp::Le => {
                if let (Some(ln), Ok(rn)) = (left.as_number(), right_str.parse::<f64>()) {
                    return ln <= rn;
                }
                left.as_str_lossy().as_str() <= right_str
            }
            CmpOp::Contains => left.as_str_lossy().contains(right_str),
            CmpOp::StartsWith => left.as_str_lossy().starts_with(right_str),
        }
    }
}
