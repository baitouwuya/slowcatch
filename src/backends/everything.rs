use anyhow::Result;

pub trait FileFinder {
    fn find(&self, query: &FindQuery) -> Result<Vec<String>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindQuery {
    pub root: Option<String>,
    pub pattern: String,
    pub files_only: bool,
    pub limit: usize,
}

#[derive(Debug, Default)]
pub struct EverythingFinder;

impl FileFinder for EverythingFinder {
    fn find(&self, query: &FindQuery) -> Result<Vec<String>> {
        find_with_everything(query)
    }
}

#[cfg(windows)]
pub fn find_with_everything(query: &FindQuery) -> Result<Vec<String>> {
    use everything_sdk::{RequestFlags, global};

    let mut everything = global()
        .try_lock()
        .map_err(|_| anyhow::anyhow!("Everything SDK global lock is busy"))?;
    if !everything.is_db_loaded()? {
        anyhow::bail!("Everything database is not loaded");
    }

    let search_text = everything_query_text(query);
    let mut searcher = everything.searcher();
    searcher
        .set_search(&search_text)
        .set_match_path(true)
        .set_max(query.limit.min(u32::MAX as usize) as u32)
        .set_request_flags(
            RequestFlags::EVERYTHING_REQUEST_FILE_NAME | RequestFlags::EVERYTHING_REQUEST_PATH,
        );

    let results = searcher.query();
    let mut paths = Vec::new();
    for item in results.iter() {
        if query.files_only && item.is_folder() {
            continue;
        }
        paths.push(item.filepath()?.display().to_string());
        if paths.len() >= query.limit {
            break;
        }
    }

    Ok(paths)
}

#[cfg(not(windows))]
pub fn find_with_everything(_query: &FindQuery) -> Result<Vec<String>> {
    anyhow::bail!("Everything SDK backend is only available on Windows")
}

fn everything_query_text(query: &FindQuery) -> String {
    let mut parts = Vec::new();
    if query.files_only {
        parts.push("file:".to_string());
    }
    parts.push(query.pattern.clone());
    if let Some(root) = &query.root {
        parts.push(root.clone());
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockFinder;

    impl FileFinder for MockFinder {
        fn find(&self, query: &FindQuery) -> Result<Vec<String>> {
            Ok(vec![format!(
                "{}:{}:{}",
                query.root.as_deref().unwrap_or("<cwd>"),
                query.pattern,
                query.files_only
            )])
        }
    }

    #[test]
    fn file_finder_trait_supports_mocking_everything() {
        let result = MockFinder
            .find(&FindQuery {
                root: Some("C:\\repo".to_string()),
                pattern: "*.rs".to_string(),
                files_only: true,
                limit: 10,
            })
            .unwrap();

        assert_eq!(result, vec!["C:\\repo:*.rs:true"]);
    }

    #[cfg(all(windows, feature = "live-everything"))]
    #[test]
    fn live_everything_can_find_cargo_toml() {
        let result = EverythingFinder
            .find(&FindQuery {
                root: Some(env!("CARGO_MANIFEST_DIR").to_string()),
                pattern: "Cargo.toml".to_string(),
                files_only: true,
                limit: 5,
            })
            .unwrap();

        assert!(
            result.iter().any(|path| path.ends_with("Cargo.toml")),
            "expected Cargo.toml in live Everything results, got {result:?}"
        );
    }
}
