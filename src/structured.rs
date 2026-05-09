use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use std::path::Path;

pub type Record = Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordOptions {
    pub select: Option<Vec<String>>,
    pub where_clause: Option<WhereClause>,
    pub sort_by: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhereClause {
    Equals { column: String, value: String },
    Contains { column: String, value: String },
}

impl WhereClause {
    pub fn parse(input: &str) -> Result<Self> {
        if let Some((column, value)) = input.split_once('~') {
            return Ok(Self::Contains {
                column: parse_column(column)?,
                value: value.to_string(),
            });
        }
        if let Some((column, value)) = input.split_once('=') {
            return Ok(Self::Equals {
                column: parse_column(column)?,
                value: value.to_string(),
            });
        }
        bail!("unsupported --where expression; use column=value or column~text")
    }
}

pub fn parse_select(input: &str) -> Result<Vec<String>> {
    let columns: Vec<String> = input
        .split(',')
        .map(str::trim)
        .filter(|column| !column.is_empty())
        .map(parse_column)
        .collect::<Result<_>>()?;
    if columns.is_empty() {
        bail!("--select must include at least one column")
    }
    Ok(columns)
}

pub fn parse_sort_by(input: &str) -> Result<String> {
    parse_column(input)
}

pub fn validate_columns(options: &RecordOptions, allowed_columns: &[&str]) -> Result<()> {
    if let Some(columns) = &options.select {
        for column in columns {
            validate_column(column, allowed_columns)?;
        }
    }
    if let Some(where_clause) = &options.where_clause {
        validate_column(where_clause.column(), allowed_columns)?;
    }
    if let Some(column) = &options.sort_by {
        validate_column(column, allowed_columns)?;
    }
    Ok(())
}

fn parse_column(column: &str) -> Result<String> {
    let trimmed = column.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        bail!("invalid column name: {column}")
    }
    Ok(trimmed.to_string())
}

fn validate_column(column: &str, allowed_columns: &[&str]) -> Result<()> {
    if allowed_columns.contains(&column) {
        return Ok(());
    }
    bail!(
        "unsupported column '{column}'; available columns: {}",
        allowed_columns.join(",")
    )
}

impl WhereClause {
    fn column(&self) -> &str {
        match self {
            WhereClause::Equals { column, .. } | WhereClause::Contains { column, .. } => column,
        }
    }
}

pub fn path_record(path: &str) -> Record {
    let path_value = Path::new(path);
    let mut record = Record::new();
    record.insert("path".to_string(), json!(path));
    record.insert(
        "name".to_string(),
        json!(
            path_value
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
        ),
    );
    record.insert(
        "extension".to_string(),
        json!(
            path_value
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| format!(".{value}"))
                .unwrap_or_default()
        ),
    );
    record.insert(
        "parent".to_string(),
        json!(
            path_value
                .parent()
                .map(|value| value.display().to_string())
                .unwrap_or_default()
        ),
    );
    record
}

pub fn grep_record(path: &Path, line_number: usize, line: &str) -> Record {
    let mut record = path_record(&path.display().to_string());
    record.insert("line_number".to_string(), json!(line_number));
    record.insert("line".to_string(), json!(line));
    record
}

pub fn slice_record(line_number: usize, text: &str) -> Record {
    let mut record = Record::new();
    record.insert("line_number".to_string(), json!(line_number));
    record.insert("text".to_string(), json!(text));
    record
}

pub fn apply_options(mut records: Vec<Record>, options: &RecordOptions) -> Vec<Record> {
    if let Some(where_clause) = &options.where_clause {
        records.retain(|record| matches_where(record, where_clause));
    }
    if let Some(column) = &options.sort_by {
        records.sort_by_key(|record| value_as_sort_key(record.get(column)));
    }
    if let Some(limit) = options.limit {
        records.truncate(limit);
    }
    if let Some(columns) = &options.select {
        records = records
            .into_iter()
            .map(|record| {
                let mut selected = Record::new();
                for column in columns {
                    selected.insert(
                        column.clone(),
                        record.get(column).cloned().unwrap_or(Value::Null),
                    );
                }
                selected
            })
            .collect();
    }
    records
}

pub fn render_records(
    records: &[Record],
    format: OutputFormat,
    text_column: Option<&str>,
) -> Result<String> {
    match format {
        OutputFormat::Json => Ok(serde_json::to_string_pretty(records)?),
        OutputFormat::Jsonl => Ok(records
            .iter()
            .map(serde_json::to_string)
            .collect::<std::result::Result<Vec<_>, _>>()?
            .join("\n")),
        OutputFormat::Text => Ok(records
            .iter()
            .map(|record| render_text_record(record, text_column))
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

fn matches_where(record: &Record, where_clause: &WhereClause) -> bool {
    match where_clause {
        WhereClause::Equals { column, value } => {
            value_as_text(record.get(column)).is_some_and(|actual| actual == *value)
        }
        WhereClause::Contains { column, value } => {
            value_as_text(record.get(column)).is_some_and(|actual| actual.contains(value))
        }
    }
}

fn value_as_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null => Some(String::new()),
        _ => None,
    }
}

fn value_as_sort_key(value: Option<&Value>) -> String {
    value_as_text(value).unwrap_or_default()
}

fn render_text_record(record: &Record, text_column: Option<&str>) -> String {
    if let Some(column) = text_column {
        if let Some(text) = value_as_text(record.get(column)) {
            return text;
        }
    }
    if record.contains_key("path")
        && record.contains_key("line_number")
        && record.contains_key("line")
    {
        return format!(
            "{}:{}:{}",
            value_as_text(record.get("path")).unwrap_or_default(),
            value_as_text(record.get("line_number")).unwrap_or_default(),
            value_as_text(record.get("line")).unwrap_or_default()
        );
    }
    value_as_text(record.get("path"))
        .unwrap_or_else(|| serde_json::to_string(record).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_records_include_file_parts() {
        let record = path_record("E:\\repo\\src\\main.rs");

        assert_eq!(record["name"], "main.rs");
        assert_eq!(record["extension"], ".rs");
        assert_eq!(record["parent"], "E:\\repo\\src");
    }

    #[test]
    fn options_filter_sort_limit_and_select_records() {
        let records = vec![
            path_record("E:\\repo\\b.txt"),
            path_record("E:\\repo\\a.rs"),
            path_record("E:\\repo\\c.rs"),
        ];
        let options = RecordOptions {
            select: Some(vec!["name".to_string(), "extension".to_string()]),
            where_clause: Some(WhereClause::Equals {
                column: "extension".to_string(),
                value: ".rs".to_string(),
            }),
            sort_by: Some("name".to_string()),
            limit: Some(1),
        };

        let output = apply_options(records, &options);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0]["name"], "a.rs");
        assert_eq!(output[0].len(), 2);
    }

    #[test]
    fn renders_jsonl() {
        let records = vec![slice_record(2, "hello")];
        let output = render_records(&records, OutputFormat::Jsonl, Some("text")).unwrap();

        assert_eq!(output, r#"{"line_number":2,"text":"hello"}"#);
    }

    #[test]
    fn grep_records_include_line_data_and_path_parts() {
        let record = grep_record(Path::new("E:\\repo\\src\\main.rs"), 7, "needle");

        assert_eq!(record["path"], "E:\\repo\\src\\main.rs");
        assert_eq!(record["name"], "main.rs");
        assert_eq!(record["extension"], ".rs");
        assert_eq!(record["line_number"], 7);
        assert_eq!(record["line"], "needle");
    }

    #[test]
    fn slice_records_include_line_number_and_text() {
        let record = slice_record(3, "three");

        assert_eq!(record["line_number"], 3);
        assert_eq!(record["text"], "three");
    }

    #[test]
    fn validates_unknown_columns() {
        let options = RecordOptions {
            sort_by: Some("missing".to_string()),
            ..Default::default()
        };

        let error = validate_columns(&options, &["path", "name"]).unwrap_err();

        assert!(error.to_string().contains("unsupported column"));
    }
}
