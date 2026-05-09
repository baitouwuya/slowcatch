use crate::shell_parse::UnknownCommand;
use anyhow::Result;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const DEDUPE_WINDOW_MS: u128 = 60_000;
static RECENT_LOG_KEYS: OnceLock<Mutex<Vec<(String, u128)>>> = OnceLock::new();

#[derive(Debug, Clone, Default)]
pub struct HookLogContext {
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, serde::Deserialize, Serialize)]
pub struct UnknownLogEntry {
    pub timestamp_unix_ms: u128,
    pub source: String,
    pub reason: String,
    pub first_command: Option<String>,
    pub pipeline_commands: Vec<String>,
    pub normalized_shape: String,
    pub command: String,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub model: Option<String>,
}

pub fn default_log_path() -> PathBuf {
    std::env::var_os("RUST_FAST_TOOL_UNKNOWN_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Users\hxf53"))
                .join(".codex")
                .join("logs")
                .join("slowcatch-unknown.jsonl")
        })
}

pub fn append_unknown_command(
    path: &Path,
    command: &str,
    detail: &UnknownCommand,
    context: &HookLogContext,
) -> Result<()> {
    let key = dedupe_key(command, detail, context);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    if should_suppress_duplicate(path, &key, now) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let entry = UnknownLogEntry {
        timestamp_unix_ms: now,
        source: "codex_hook".to_string(),
        reason: detail.reason.clone(),
        first_command: detail.first_command.clone(),
        pipeline_commands: detail.pipeline_commands.clone(),
        normalized_shape: detail.normalized_shape.clone(),
        command: command.to_string(),
        cwd: context.cwd.clone(),
        session_id: context.session_id.clone(),
        turn_id: context.turn_id.clone(),
        model: context.model.clone(),
    };

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", serde_json::to_string(&entry)?)?;
    Ok(())
}

fn dedupe_key(command: &str, detail: &UnknownCommand, context: &HookLogContext) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        detail.reason,
        detail.normalized_shape,
        command,
        context.cwd.as_deref().unwrap_or_default()
    )
}

fn should_suppress_duplicate(path: &Path, key: &str, now: u128) -> bool {
    let recent = RECENT_LOG_KEYS.get_or_init(|| Mutex::new(Vec::new()));
    let Ok(mut entries) = recent.lock() else {
        return false;
    };
    entries.retain(|(_, timestamp)| now.saturating_sub(*timestamp) <= DEDUPE_WINDOW_MS);
    if entries.iter().any(|(existing, _)| existing == key)
        || persisted_recent_duplicate(path, key, now)
    {
        return true;
    }
    entries.push((key.to_string(), now));
    false
}

fn persisted_recent_duplicate(path: &Path, key: &str, now: u128) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let reader = BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<UnknownLogEntry>(&line) else {
            continue;
        };
        if now.saturating_sub(entry.timestamp_unix_ms) > DEDUPE_WINDOW_MS {
            continue;
        }
        if entry_key(&entry) == key {
            return true;
        }
    }
    false
}

fn entry_key(entry: &UnknownLogEntry) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        entry.reason,
        entry.normalized_shape,
        entry.command,
        entry.cwd.as_deref().unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_unknown_log_entries_are_suppressed_in_window() {
        let temp = tempfile::tempdir().unwrap();
        let log_path = temp.path().join("unknown.jsonl");
        let detail = UnknownCommand {
            reason: "same reason".to_string(),
            first_command: Some("get-content".to_string()),
            pipeline_commands: vec!["get-content".to_string()],
            normalized_shape: "same shape".to_string(),
        };
        let context = HookLogContext {
            cwd: Some("C:\\repo".to_string()),
            ..HookLogContext::default()
        };

        append_unknown_command(&log_path, "Get-Content file.txt", &detail, &context).unwrap();
        append_unknown_command(&log_path, "Get-Content file.txt", &detail, &context).unwrap();

        let lines = std::fs::read_to_string(log_path).unwrap();
        assert_eq!(lines.lines().count(), 1);
    }

    #[test]
    fn persisted_duplicate_unknown_log_entries_are_suppressed() {
        let temp = tempfile::tempdir().unwrap();
        let log_path = temp.path().join("unknown.jsonl");
        let detail = UnknownCommand {
            reason: "persisted reason".to_string(),
            first_command: Some("get-content".to_string()),
            pipeline_commands: vec!["get-content".to_string()],
            normalized_shape: "persisted shape".to_string(),
        };
        let context = HookLogContext {
            cwd: Some("C:\\repo".to_string()),
            ..HookLogContext::default()
        };
        let entry = UnknownLogEntry {
            timestamp_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
            source: "codex_hook".to_string(),
            reason: detail.reason.clone(),
            first_command: detail.first_command.clone(),
            pipeline_commands: detail.pipeline_commands.clone(),
            normalized_shape: detail.normalized_shape.clone(),
            command: "Get-Content file.txt".to_string(),
            cwd: context.cwd.clone(),
            session_id: None,
            turn_id: None,
            model: None,
        };
        std::fs::write(
            &log_path,
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();

        append_unknown_command(&log_path, "Get-Content file.txt", &detail, &context).unwrap();

        let lines = std::fs::read_to_string(log_path).unwrap();
        assert_eq!(lines.lines().count(), 1);
    }
}
