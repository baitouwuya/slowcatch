use crate::backends::everything::FindQuery;
use anyhow::{Result, bail};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Projection {
    FullNameOnly,
    Metadata { fields: Vec<MetadataField> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataField {
    FullName,
    Name,
    Length,
    LastWriteTime,
    Mode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindProjection {
    pub query: FindQuery,
    pub projection: Projection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlyExternal {
    pub program: String,
    pub args: Vec<String>,
}

pub fn render_projection(paths: &[String], projection: &Projection) -> Result<String> {
    let capped = paths.iter().take(200);
    let mut lines = Vec::new();
    match projection {
        Projection::FullNameOnly => {
            lines.extend(capped.cloned());
        }
        Projection::Metadata { fields } => {
            for path in capped {
                lines.push(render_metadata_line(path, fields));
            }
        }
    }
    if paths.len() > 200 {
        lines.push(format!("omitted_count={}", paths.len() - 200));
    }
    Ok(lines.join("\n"))
}

pub fn run_read_only_external(external: &ReadOnlyExternal, cwd: Option<&str>) -> Result<String> {
    validate_read_only_external(external)?;
    let mut child = Command::new(&external.program)
        .args(&external.args)
        .current_dir(cwd.unwrap_or("."))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            if let Some(mut handle) = child.stdout.take() {
                handle.read_to_end(&mut stdout)?;
            }
            if let Some(mut handle) = child.stderr.take() {
                handle.read_to_end(&mut stderr)?;
            }
            if !status.success() {
                bail!("external command exited with {status}");
            }
            if !stderr.is_empty() {
                bail!("external command wrote stderr");
            }
            if stdout.len() > 64 * 1024 {
                bail!("external command stdout exceeded 64KiB");
            }
            return Ok(String::from_utf8(stdout)?);
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            bail!("external command timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn normalize_path_for_metadata(path: &str) -> PathBuf {
    PathBuf::from(path)
}

fn render_metadata_line(path: &str, fields: &[MetadataField]) -> String {
    let path_buf = normalize_path_for_metadata(path);
    let metadata = std::fs::metadata(&path_buf).ok();
    fields
        .iter()
        .map(|field| match field {
            MetadataField::FullName => format!("FullName={}", path_buf.display()),
            MetadataField::Name => format!(
                "Name={}",
                path_buf
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default()
            ),
            MetadataField::Length => format!(
                "Length={}",
                metadata
                    .as_ref()
                    .map(|value| value.len().to_string())
                    .unwrap_or_default()
            ),
            MetadataField::LastWriteTime => format!(
                "LastWriteTime={}",
                metadata
                    .as_ref()
                    .and_then(|value| value.modified().ok())
                    .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|value| value.as_secs().to_string())
                    .unwrap_or_default()
            ),
            MetadataField::Mode => format!(
                "Mode={}",
                if metadata.as_ref().is_some_and(|value| value.is_dir()) {
                    "d"
                } else {
                    "-"
                }
            ),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_read_only_external(external: &ReadOnlyExternal) -> Result<()> {
    if external.program != "git" {
        bail!("external program is not allowlisted");
    }
    let args = external.args.as_slice();
    match args {
        [status, flag] if status == "status" && flag == "--short" => Ok(()),
        [status, flag] if status == "status" && flag == "--porcelain" => Ok(()),
        [status, flag] if status == "status" && flag == "--porcelain=v1" => Ok(()),
        [status, flag] if status == "status" && flag == "--porcelain=v2" => Ok(()),
        [diff, flag] if diff == "diff" && flag == "--stat" => Ok(()),
        [log, oneline, nflag, value] if log == "log" && oneline == "--oneline" && nflag == "-n" => {
            let count: usize = value.parse()?;
            if (1..=50).contains(&count) {
                Ok(())
            } else {
                bail!("git log count is outside 1..=50")
            }
        }
        _ => bail!("external command is not allowlisted"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullname_projection_outputs_paths() {
        let output = render_projection(
            &["C:\\repo\\AGENTS.md".to_string()],
            &Projection::FullNameOnly,
        )
        .unwrap();

        assert_eq!(output, "C:\\repo\\AGENTS.md");
    }

    #[test]
    fn metadata_projection_outputs_key_value_lines() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.txt");
        std::fs::write(&file, "hello").unwrap();
        let output = render_projection(
            &[file.display().to_string()],
            &Projection::Metadata {
                fields: vec![MetadataField::FullName, MetadataField::Length],
            },
        )
        .unwrap();

        assert!(output.contains("FullName="));
        assert!(output.contains("Length=5"));
    }

    #[test]
    fn readonly_git_status_allowlist_is_direct_exec() {
        let external = ReadOnlyExternal {
            program: "git".to_string(),
            args: vec!["status".to_string(), "--short".to_string()],
        };

        let output = run_read_only_external(&external, Some(env!("CARGO_MANIFEST_DIR"))).unwrap();

        assert!(output.len() <= 64 * 1024);
    }

    #[test]
    fn non_allowlisted_external_fails() {
        let external = ReadOnlyExternal {
            program: "git".to_string(),
            args: vec!["checkout".to_string(), "main".to_string()],
        };

        assert!(run_read_only_external(&external, Some(env!("CARGO_MANIFEST_DIR"))).is_err());
    }
}
