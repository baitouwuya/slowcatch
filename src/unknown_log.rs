use crate::shell_parse::UnknownCommand;
use anyhow::Result;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Default)]
pub struct HookLogContext {
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UnknownLogEntry {
    pub timestamp_unix_ms: u128,
    pub source: &'static str,
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
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let entry = UnknownLogEntry {
        timestamp_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        source: "codex_hook",
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
