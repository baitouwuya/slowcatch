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
pub struct FindPipeline {
    pub query: FindQuery,
    pub where_filter: Option<FileWhere>,
    pub sort: Option<FileSort>,
    pub projection: Projection,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindMeasure {
    pub query: FindQuery,
    pub where_filter: Option<FileWhere>,
    pub measure: MeasureKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasureKind {
    Count,
    Length,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileWhere {
    Like { field: FileField, pattern: String },
    Equals { field: FileField, value: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSort {
    pub field: FileField,
    pub descending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileField {
    FullName,
    Name,
    Extension,
    Length,
    LastWriteTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOnlyExternal {
    pub program: String,
    pub args: Vec<String>,
}

pub fn render_projection(paths: &[String], projection: &Projection) -> Result<String> {
    if paths.is_empty() {
        return Ok("no results".to_string());
    }
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

pub fn apply_find_pipeline(mut paths: Vec<String>, pipeline: &FindPipeline) -> Vec<String> {
    if let Some(filter) = &pipeline.where_filter {
        paths.retain(|path| matches_file_where(path, filter));
    }
    if let Some(sort) = &pipeline.sort {
        paths.sort_by_key(|path| sort_key(path, sort.field));
        if sort.descending {
            paths.reverse();
        }
    }
    if let Some(limit) = pipeline.limit {
        paths.truncate(limit);
    }
    paths
}

pub fn render_find_measure(mut paths: Vec<String>, measure: &FindMeasure) -> String {
    if let Some(filter) = &measure.where_filter {
        paths.retain(|path| matches_file_where(path, filter));
    }
    match measure.measure {
        MeasureKind::Count => format!("Count={}", paths.len()),
        MeasureKind::Length => {
            let mut count = 0usize;
            let mut sum = 0u64;
            let mut min = None;
            let mut max = None;
            for path in paths {
                if let Ok(metadata) = std::fs::metadata(path) {
                    let length = metadata.len();
                    count += 1;
                    sum += length;
                    min = Some(min.map_or(length, |value: u64| value.min(length)));
                    max = Some(max.map_or(length, |value: u64| value.max(length)));
                }
            }
            format!(
                "Count={count} Sum={sum} Minimum={} Maximum={}",
                min.map(|value| value.to_string()).unwrap_or_default(),
                max.map(|value| value.to_string()).unwrap_or_default()
            )
        }
    }
}

fn matches_file_where(path: &str, filter: &FileWhere) -> bool {
    match filter {
        FileWhere::Like { field, pattern } => wildcard_match(
            &field_value(path, *field).to_ascii_lowercase(),
            &pattern.to_ascii_lowercase(),
        ),
        FileWhere::Equals { field, value } => field_value(path, *field).eq_ignore_ascii_case(value),
    }
}

fn wildcard_match(value: &str, pattern: &str) -> bool {
    let escaped = regex::escape(pattern)
        .replace("\\*", ".*")
        .replace("\\?", ".");
    regex::Regex::new(&format!("^{escaped}$"))
        .map(|regex| regex.is_match(value))
        .unwrap_or(false)
}

fn sort_key(path: &str, field: FileField) -> String {
    match field {
        FileField::Length => std::fs::metadata(path)
            .map(|metadata| format!("{:020}", metadata.len()))
            .unwrap_or_default(),
        FileField::LastWriteTime => std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| format!("{:020}", duration.as_secs()))
            .unwrap_or_default(),
        _ => field_value(path, field).to_ascii_lowercase(),
    }
}

fn field_value(path: &str, field: FileField) -> String {
    let path_buf = PathBuf::from(path);
    match field {
        FileField::FullName => path_buf.display().to_string(),
        FileField::Name => path_buf
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string(),
        FileField::Extension => path_buf
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| format!(".{value}"))
            .unwrap_or_default(),
        FileField::Length => std::fs::metadata(&path_buf)
            .map(|metadata| metadata.len().to_string())
            .unwrap_or_default(),
        FileField::LastWriteTime => std::fs::metadata(&path_buf)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().to_string())
            .unwrap_or_default(),
    }
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
    fn empty_projection_outputs_no_results() {
        let output = render_projection(&[], &Projection::FullNameOnly).unwrap();

        assert_eq!(output, "no results");
    }

    #[test]
    fn find_pipeline_filters_sorts_and_limits_paths() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old.rs");
        let new = temp.path().join("new.rs");
        let text = temp.path().join("note.txt");
        std::fs::write(&old, "old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&new, "newer").unwrap();
        std::fs::write(&text, "text").unwrap();
        let paths = vec![
            old.display().to_string(),
            text.display().to_string(),
            new.display().to_string(),
        ];
        let pipeline = FindPipeline {
            query: FindQuery {
                root: Some(temp.path().display().to_string()),
                pattern: "*".to_string(),
                files_only: true,
                limit: 200,
            },
            where_filter: Some(FileWhere::Like {
                field: FileField::Name,
                pattern: "*.rs".to_string(),
            }),
            sort: Some(FileSort {
                field: FileField::LastWriteTime,
                descending: true,
            }),
            projection: Projection::FullNameOnly,
            limit: Some(1),
        };

        let output = apply_find_pipeline(paths, &pipeline);

        assert_eq!(output, vec![new.display().to_string()]);
    }

    #[test]
    fn find_measure_counts_filtered_paths() {
        let temp = tempfile::tempdir().unwrap();
        let rs = temp.path().join("main.rs");
        let txt = temp.path().join("note.txt");
        std::fs::write(&rs, "rust").unwrap();
        std::fs::write(&txt, "text").unwrap();
        let measure = FindMeasure {
            query: FindQuery {
                root: Some(temp.path().display().to_string()),
                pattern: "*".to_string(),
                files_only: true,
                limit: 200,
            },
            where_filter: Some(FileWhere::Equals {
                field: FileField::Extension,
                value: ".rs".to_string(),
            }),
            measure: MeasureKind::Count,
        };

        let output = render_find_measure(
            vec![rs.display().to_string(), txt.display().to_string()],
            &measure,
        );

        assert_eq!(output, "Count=1");
    }

    #[test]
    fn find_measure_sums_lengths() {
        let temp = tempfile::tempdir().unwrap();
        let one = temp.path().join("one.rs");
        let two = temp.path().join("two.rs");
        std::fs::write(&one, "abc").unwrap();
        std::fs::write(&two, "abcde").unwrap();
        let measure = FindMeasure {
            query: FindQuery {
                root: Some(temp.path().display().to_string()),
                pattern: "*".to_string(),
                files_only: true,
                limit: 200,
            },
            where_filter: None,
            measure: MeasureKind::Length,
        };

        let output = render_find_measure(
            vec![one.display().to_string(), two.display().to_string()],
            &measure,
        );

        assert!(output.contains("Count=2"));
        assert!(output.contains("Sum=8"));
        assert!(output.contains("Minimum=3"));
        assert!(output.contains("Maximum=5"));
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
