use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const STATE_FILE_NAME: &str = "post-edit-state.jsonl";
const MAX_STATE_AGE_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PostEditEntry {
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub cwd: Option<String>,
    pub path: String,
    pub timestamp_unix: u64,
}

pub fn default_state_path() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".codex")
        .join("logs")
        .join("slowcatch")
        .join(STATE_FILE_NAME)
}

pub fn append_touched_paths(
    state_path: &Path,
    session_id: Option<&str>,
    turn_id: Option<&str>,
    cwd: Option<&str>,
    paths: &[PathBuf],
) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_path)?;
    let now = unix_now();
    for path in paths {
        let entry = PostEditEntry {
            session_id: session_id.map(ToOwned::to_owned),
            turn_id: turn_id.map(ToOwned::to_owned),
            cwd: cwd.map(ToOwned::to_owned),
            path: path.display().to_string(),
            timestamp_unix: now,
        };
        writeln!(file, "{}", serde_json::to_string(&entry)?)?;
    }
    Ok(())
}

pub fn read_touched_paths(
    state_path: &Path,
    session_id: Option<&str>,
    turn_id: Option<&str>,
) -> Result<Vec<PathBuf>> {
    let Ok(text) = fs::read_to_string(state_path) else {
        return Ok(Vec::new());
    };
    let now = unix_now();
    let mut paths = BTreeSet::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<PostEditEntry>(line) else {
            continue;
        };
        if entry.session_id.as_deref() != session_id || entry.turn_id.as_deref() != turn_id {
            continue;
        }
        if now.saturating_sub(entry.timestamp_unix) > MAX_STATE_AGE_SECONDS {
            continue;
        }
        paths.insert(PathBuf::from(entry.path));
    }
    Ok(paths.into_iter().collect())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_reader_filters_session_turn_and_dedupes_paths() {
        let temp = tempfile::tempdir().unwrap();
        let state_path = temp.path().join("state.jsonl");
        append_touched_paths(
            &state_path,
            Some("s1"),
            Some("t1"),
            Some("C:\\repo"),
            &[
                PathBuf::from("C:\\repo\\a.rs"),
                PathBuf::from("C:\\repo\\a.rs"),
            ],
        )
        .unwrap();
        append_touched_paths(
            &state_path,
            Some("s1"),
            Some("t2"),
            Some("C:\\repo"),
            &[PathBuf::from("C:\\repo\\b.rs")],
        )
        .unwrap();

        let paths = read_touched_paths(&state_path, Some("s1"), Some("t1")).unwrap();

        assert_eq!(paths, vec![PathBuf::from("C:\\repo\\a.rs")]);
    }

    #[test]
    fn stale_entries_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let state_path = temp.path().join("state.jsonl");
        let entry = PostEditEntry {
            session_id: Some("s1".to_string()),
            turn_id: Some("t1".to_string()),
            cwd: Some("C:\\repo".to_string()),
            path: "C:\\repo\\old.rs".to_string(),
            timestamp_unix: 1,
        };
        fs::write(&state_path, serde_json::to_string(&entry).unwrap()).unwrap();

        let paths = read_touched_paths(&state_path, Some("s1"), Some("t1")).unwrap();

        assert!(paths.is_empty());
    }
}
