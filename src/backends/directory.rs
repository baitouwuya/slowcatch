use crate::backends::projection::{self, Projection};
use anyhow::{Result, bail};
use regex::Regex;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryList {
    pub root: PathBuf,
    pub filter: Option<String>,
    pub names_only: bool,
    pub files_only: bool,
    pub directories_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryPipeline {
    pub list: DirectoryList,
    pub name_match: Option<DirectoryNameMatch>,
    pub projection: Projection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryNameMatch {
    pub field: DirectoryMatchField,
    pub pattern: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryMatchField {
    Name,
    FullName,
}

pub fn list_directory(options: &DirectoryList) -> Result<String> {
    let lines = collect_directory_entries(options)?;
    if lines.is_empty() {
        Ok("no results".to_string())
    } else {
        Ok(lines.join("\n"))
    }
}

pub fn list_directory_projection(pipeline: &DirectoryPipeline) -> Result<String> {
    let mut paths = collect_directory_paths(&pipeline.list)?;
    if let Some(name_match) = &pipeline.name_match {
        paths.retain(|path| directory_name_matches(path, name_match));
    }
    projection::render_projection(&paths, &pipeline.projection)
}

fn collect_directory_entries(options: &DirectoryList) -> Result<Vec<String>> {
    let paths = collect_directory_paths(options)?;
    let mut lines = Vec::new();
    for path in paths {
        if options.names_only {
            let name = PathBuf::from(&path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or(path);
            lines.push(name);
        } else {
            lines.push(path);
        }
    }
    Ok(lines)
}

fn collect_directory_paths(options: &DirectoryList) -> Result<Vec<String>> {
    let metadata = std::fs::metadata(&options.root)?;
    if !metadata.is_dir() {
        bail!("not a directory: {}", options.root.display());
    }

    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&options.root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if options.files_only && !file_type.is_file() {
            continue;
        }
        if options.directories_only && !file_type.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(filter) = &options.filter
            && !wildcard_match(&name, filter)
        {
            continue;
        }

        paths.push(entry.path().display().to_string());
    }
    paths.sort_by_key(|line| line.to_ascii_lowercase());
    Ok(paths)
}

fn directory_name_matches(path: &str, name_match: &DirectoryNameMatch) -> bool {
    let value = match name_match.field {
        DirectoryMatchField::FullName => path.to_string(),
        DirectoryMatchField::Name => PathBuf::from(path)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
    };
    Regex::new(&format!("(?i){}", name_match.pattern))
        .map(|regex| regex.is_match(&value))
        .unwrap_or(false)
}

fn wildcard_match(value: &str, pattern: &str) -> bool {
    let escaped = regex::escape(pattern)
        .replace("\\*", ".*")
        .replace("\\?", ".");
    Regex::new(&format!("(?i)^{escaped}$"))
        .map(|regex| regex.is_match(value))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn directory_listing_outputs_full_paths_by_default() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("b.txt"), "b").unwrap();
        fs::write(temp.path().join("a.txt"), "a").unwrap();

        let output = list_directory(&DirectoryList {
            root: temp.path().to_path_buf(),
            filter: None,
            names_only: false,
            files_only: false,
            directories_only: false,
        })
        .unwrap();

        assert!(output.contains(&temp.path().join("a.txt").display().to_string()));
        assert!(output.contains(&temp.path().join("b.txt").display().to_string()));
    }

    #[test]
    fn directory_listing_can_filter_and_emit_names_only() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("keep.uid"), "uid").unwrap();
        fs::write(temp.path().join("skip.txt"), "txt").unwrap();

        let output = list_directory(&DirectoryList {
            root: temp.path().to_path_buf(),
            filter: Some("*.uid".to_string()),
            names_only: true,
            files_only: true,
            directories_only: false,
        })
        .unwrap();

        assert_eq!(output, "keep.uid");
    }

    #[test]
    fn directory_projection_can_filter_directory_names() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("View3D")).unwrap();
        fs::create_dir(temp.path().join("Other")).unwrap();

        let output = list_directory_projection(&DirectoryPipeline {
            list: DirectoryList {
                root: temp.path().to_path_buf(),
                filter: None,
                names_only: false,
                files_only: false,
                directories_only: true,
            },
            name_match: Some(DirectoryNameMatch {
                field: DirectoryMatchField::Name,
                pattern: "view3d".to_string(),
            }),
            projection: Projection::FullNameOnly,
        })
        .unwrap();

        assert!(output.contains("View3D"));
        assert!(!output.contains("Other"));
    }
}
