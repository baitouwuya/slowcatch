use anyhow::Result;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepQuery {
    pub roots: Vec<PathBuf>,
    pub pattern: String,
    pub case_sensitive: bool,
    pub simple_match: bool,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepMatch {
    pub path: PathBuf,
    pub line_number: usize,
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepContextQuery {
    pub roots: Vec<PathBuf>,
    pub pattern: String,
    pub before: usize,
    pub after: usize,
    pub case_sensitive: bool,
    pub simple_match: bool,
    pub limit: usize,
}

impl GrepMatch {
    pub fn to_tool_text(&self) -> String {
        format!("{}:{}:{}", self.path.display(), self.line_number, self.line)
    }
}

pub fn grep(query: &GrepQuery) -> Result<Vec<GrepMatch>> {
    let mut matches = Vec::new();
    for root in &query.roots {
        visit_path(root, query, &mut matches)?;
        if matches.len() >= query.limit {
            break;
        }
    }
    Ok(matches)
}

pub fn grep_context(query: &GrepContextQuery) -> Result<String> {
    let matcher = ContextMatcher::new(query)?;
    let mut blocks = Vec::new();
    for root in &query.roots {
        if !root.exists() {
            anyhow::bail!("grep context root does not exist: {}", root.display());
        }
        visit_context_path(root, query, &matcher, &mut blocks)?;
        if blocks.len() >= query.limit {
            break;
        }
    }
    Ok(blocks.join("\n--\n"))
}

fn visit_path(path: &Path, query: &GrepQuery, matches: &mut Vec<GrepMatch>) -> Result<()> {
    if matches.len() >= query.limit {
        return Ok(());
    }

    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            visit_path(&entry.path(), query, matches)?;
            if matches.len() >= query.limit {
                break;
            }
        }
        return Ok(());
    }

    if !path.is_file() {
        return Ok(());
    }

    let Ok(text) = fs::read_to_string(path) else {
        return Ok(());
    };

    for (index, line) in text.lines().enumerate() {
        if line_matches(line, query) {
            matches.push(GrepMatch {
                path: path.to_path_buf(),
                line_number: index + 1,
                line: line.to_string(),
            });
            if matches.len() >= query.limit {
                break;
            }
        }
    }

    Ok(())
}

fn visit_context_path(
    path: &Path,
    query: &GrepContextQuery,
    matcher: &ContextMatcher,
    blocks: &mut Vec<String>,
) -> Result<()> {
    if blocks.len() >= query.limit {
        return Ok(());
    }
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            visit_context_path(&entry?.path(), query, matcher, blocks)?;
            if blocks.len() >= query.limit {
                break;
            }
        }
        return Ok(());
    }
    if !path.is_file() {
        return Ok(());
    }
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(());
    };
    let lines: Vec<&str> = text.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if matcher.is_match(line) {
            let start = index.saturating_sub(query.before);
            let end = (index + query.after + 1).min(lines.len());
            let mut block = Vec::new();
            for (line_index, value) in lines.iter().enumerate().take(end).skip(start) {
                let kind = if line_index == index {
                    "match"
                } else if line_index < index {
                    "before"
                } else {
                    "after"
                };
                block.push(format!(
                    "{}:L{}:{kind}: {}",
                    path.display(),
                    line_index + 1,
                    value
                ));
            }
            blocks.push(block.join("\n"));
            if blocks.len() >= query.limit {
                break;
            }
        }
    }
    Ok(())
}

enum ContextMatcher {
    Regex(Regex),
    Literal {
        pattern: String,
        case_sensitive: bool,
    },
}

impl ContextMatcher {
    fn new(query: &GrepContextQuery) -> Result<Self> {
        if query.simple_match {
            return Ok(Self::Literal {
                pattern: if query.case_sensitive {
                    query.pattern.clone()
                } else {
                    query.pattern.to_lowercase()
                },
                case_sensitive: query.case_sensitive,
            });
        }
        let pattern = if query.case_sensitive {
            query.pattern.clone()
        } else {
            format!("(?i){}", query.pattern)
        };
        Ok(Self::Regex(Regex::new(&pattern)?))
    }

    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Regex(regex) => regex.is_match(line),
            Self::Literal {
                pattern,
                case_sensitive,
            } => {
                if *case_sensitive {
                    line.contains(pattern)
                } else {
                    line.to_lowercase().contains(pattern)
                }
            }
        }
    }
}

fn line_matches(line: &str, query: &GrepQuery) -> bool {
    if query.case_sensitive {
        line.contains(&query.pattern)
    } else {
        line.to_lowercase().contains(&query.pattern.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn grep_emits_path_line_text() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.txt");
        let mut handle = fs::File::create(&file).unwrap();
        writeln!(handle, "alpha").unwrap();
        writeln!(handle, "needle here").unwrap();

        let matches = grep(&GrepQuery {
            roots: vec![temp.path().to_path_buf()],
            pattern: "needle".to_string(),
            case_sensitive: true,
            simple_match: true,
            limit: 10,
        })
        .unwrap();

        assert_eq!(matches.len(), 1);
        assert!(matches[0].to_tool_text().ends_with(":2:needle here"));
    }

    #[test]
    fn grep_context_outputs_before_match_after_lines() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.txt");
        fs::write(&file, "before\nneedle\nnext\n").unwrap();

        let output = grep_context(&GrepContextQuery {
            roots: vec![file],
            pattern: "needle".to_string(),
            before: 1,
            after: 1,
            case_sensitive: true,
            simple_match: true,
            limit: 10,
        })
        .unwrap();

        assert!(output.contains("L1:before: before"));
        assert!(output.contains("L2:match: needle"));
        assert!(output.contains("L3:after: next"));
    }

    #[test]
    fn grep_context_invalid_regex_fails() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.txt");
        fs::write(&file, "needle\n").unwrap();

        let result = grep_context(&GrepContextQuery {
            roots: vec![file],
            pattern: "[".to_string(),
            before: 0,
            after: 0,
            case_sensitive: true,
            simple_match: false,
            limit: 10,
        });

        assert!(result.is_err());
    }
}
