use anyhow::{Context, Result};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser};

const MAX_FILES_TO_CHECK: usize = 20;
const MAX_FILE_BYTES: u64 = 1_000_000;
const MAX_FINDINGS: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostEditReport {
    findings: Vec<Finding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Finding {
    path: PathBuf,
    line: Option<usize>,
    severity: Severity,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuickCheckLanguage {
    Rust,
    Python,
    JavaScriptLike,
    JsxLike,
    CLike,
    Go,
    JavaLike,
    CSharp,
    GDScript,
    Json,
    Toml,
    Yaml,
    CssLike,
    HtmlXml,
    Markdown,
    ShellLike,
    Unknown,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

impl PostEditReport {
    fn new(findings: Vec<Finding>) -> Self {
        Self { findings }
    }

    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn render(&self) -> String {
        let mut output = vec![
            "Edited files were already changed on disk. Slowcatch found issues to fix before continuing:".to_string(),
        ];
        for finding in &self.findings {
            let location = match finding.line {
                Some(line) => format!("{}:L{line}", finding.path.display()),
                None => finding.path.display().to_string(),
            };
            output.push(format!(
                "{} {} {}",
                finding.severity.as_str(),
                location,
                finding.message
            ));
        }
        output.join("\n")
    }
}

pub fn check_apply_patch_command(command: &str, cwd: Option<&str>) -> Option<PostEditReport> {
    let paths = touched_paths_from_apply_patch(command, cwd)?;
    let mut findings = Vec::new();
    for path in paths {
        if findings.len() >= MAX_FINDINGS {
            break;
        }
        let Ok(mut file_findings) = check_file(&path) else {
            continue;
        };
        findings.append(&mut file_findings);
    }
    findings.truncate(MAX_FINDINGS);
    let report = PostEditReport::new(findings);
    if report.is_empty() {
        None
    } else {
        Some(report)
    }
}

pub fn touched_paths_from_apply_patch(command: &str, cwd: Option<&str>) -> Option<Vec<PathBuf>> {
    extract_apply_patch_paths(command, cwd)
}

fn extract_apply_patch_paths(command: &str, cwd: Option<&str>) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut deleted = HashSet::new();
    let mut seen = HashSet::new();

    for line in command.lines() {
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            if let Some(path) = resolve_patch_path(path, cwd) {
                deleted.insert(path);
            }
            continue;
        }

        let path = line
            .strip_prefix("*** Add File: ")
            .or_else(|| line.strip_prefix("*** Update File: "))
            .or_else(|| line.strip_prefix("*** Move to: "));
        if let Some(path) = path.and_then(|path| resolve_patch_path(path, cwd)) {
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
    }

    paths.retain(|path| !deleted.contains(path));
    if paths.is_empty() {
        None
    } else {
        paths.truncate(MAX_FILES_TO_CHECK);
        Some(paths)
    }
}

fn resolve_patch_path(path: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let path = path.trim();
    if path.is_empty() || path.contains('\0') {
        return None;
    }
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Some(path)
    } else {
        let cwd = cwd?;
        Some(Path::new(cwd).join(path))
    }
}

fn check_file(path: &Path) -> Result<Vec<Finding>> {
    let metadata = fs::metadata(path).with_context(|| format!("metadata {}", path.display()))?;
    let language = language_for_path(path);
    if !metadata.is_file()
        || metadata.len() > MAX_FILE_BYTES
        || language == QuickCheckLanguage::Unknown
    {
        return Ok(Vec::new());
    }

    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.contains(&0) {
        return Ok(Vec::new());
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return Ok(Vec::new()),
    };

    let mut findings = Vec::new();
    let scan_text = mask_for_language(language, &text);
    findings.extend(check_line_markers(path, &scan_text));
    if language.allows_delimiter_check() {
        findings.extend(check_balanced_delimiters(path, &scan_text));
    }
    if language == QuickCheckLanguage::Rust {
        findings.extend(check_rust_debug_macros(path, &scan_text));
    }
    if !has_delimiter_error(&findings) {
        if let Some(syntax_finding) = check_syntax_parse(path, &text) {
            findings.push(syntax_finding);
        }
    }
    Ok(findings)
}

fn check_line_markers(path: &Path, text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let message =
            if line.contains("<<<<<<<") || line.contains("=======") || line.contains(">>>>>>>") {
                Some(("git conflict marker", Severity::Error))
            } else if line.contains("*** Begin Patch") || line.contains("*** End Patch") {
                Some(("patch marker left in file", Severity::Error))
            } else if line.contains("TODO_REMOVE") || line.contains("FIXME_REMOVE") {
                Some(("remove placeholder before continuing", Severity::Error))
            } else {
                None
            };
        if let Some((message, severity)) = message {
            findings.push(Finding {
                path: path.to_path_buf(),
                line: Some(line_number),
                severity,
                message: message.to_string(),
            });
        }
    }
    findings
}

fn check_balanced_delimiters(path: &Path, text: &str) -> Vec<Finding> {
    let mut stack: VecDeque<(char, usize)> = VecDeque::new();
    let mut line = 1usize;

    for ch in text.chars() {
        if ch == '\n' {
            line += 1;
            continue;
        }
        match ch {
            '{' | '[' | '(' => stack.push_back((ch, line)),
            '}' | ']' | ')' => {
                let Some((open, open_line)) = stack.pop_back() else {
                    return vec![Finding {
                        path: path.to_path_buf(),
                        line: Some(line),
                        severity: Severity::Error,
                        message: format!("unmatched closing delimiter `{ch}`"),
                    }];
                };
                if !matches_delimiter(open, ch) {
                    return vec![Finding {
                        path: path.to_path_buf(),
                        line: Some(line),
                        severity: Severity::Error,
                        message: format!(
                            "mismatched delimiter `{open}` from L{open_line} closed by `{ch}`"
                        ),
                    }];
                }
            }
            _ => {}
        }
    }

    if let Some((open, open_line)) = stack.pop_back() {
        return vec![Finding {
            path: path.to_path_buf(),
            line: Some(open_line),
            severity: Severity::Error,
            message: format!("unclosed delimiter `{open}`"),
        }];
    }

    Vec::new()
}

fn check_rust_debug_macros(path: &Path, text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let message = if line.contains("dbg!(") {
            Some("debug macro `dbg!` left in Rust file")
        } else if line.contains("todo!()") {
            Some("placeholder macro `todo!()` left in Rust file")
        } else if line.contains("unimplemented!()") {
            Some("placeholder macro `unimplemented!()` left in Rust file")
        } else {
            None
        };
        if let Some(message) = message {
            findings.push(Finding {
                path: path.to_path_buf(),
                line: Some(index + 1),
                severity: Severity::Warning,
                message: message.to_string(),
            });
        }
    }
    findings
}

fn check_syntax_parse(path: &Path, text: &str) -> Option<Finding> {
    let tree_sitter_language = language_for_path(path).tree_sitter_language(path)?;
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_language).ok()?;
    let tree = parser.parse(text, None)?;
    let line = first_syntax_issue_line(tree.root_node())?;
    Some(Finding {
        path: path.to_path_buf(),
        line: Some(line),
        severity: Severity::Warning,
        message: "syntax parse issue detected".to_string(),
    })
}

fn first_syntax_issue_line(node: Node<'_>) -> Option<usize> {
    if !node.has_error() && !node.is_error() && !node.is_missing() {
        return None;
    }
    if node.is_error() || node.is_missing() {
        return Some(node.start_position().row + 1);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(line) = first_syntax_issue_line(child) {
            return Some(line);
        }
    }
    Some(node.start_position().row + 1)
}

fn has_delimiter_error(findings: &[Finding]) -> bool {
    findings.iter().any(|finding| {
        finding.severity == Severity::Error
            && (finding.message.contains("delimiter")
                || finding.message.contains("unclosed delimiter"))
    })
}

impl QuickCheckLanguage {
    fn allows_delimiter_check(self) -> bool {
        matches!(
            self,
            Self::Rust
                | Self::JavaScriptLike
                | Self::CLike
                | Self::Go
                | Self::JavaLike
                | Self::CSharp
                | Self::GDScript
                | Self::Json
                | Self::CssLike
        )
    }

    fn tree_sitter_language(self, path: &Path) -> Option<tree_sitter::Language> {
        match self {
            Self::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
            Self::Python => Some(tree_sitter_python::LANGUAGE.into()),
            Self::GDScript => Some(tree_sitter_gdscript::LANGUAGE.into()),
            Self::CLike => match lowercase_extension(path).as_deref() {
                Some("c") => Some(tree_sitter_c::LANGUAGE.into()),
                Some("cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx") => {
                    Some(tree_sitter_cpp::LANGUAGE.into())
                }
                _ => None,
            },
            _ => None,
        }
    }
}

fn lowercase_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
}

fn mask_for_language(language: QuickCheckLanguage, text: &str) -> String {
    match language {
        QuickCheckLanguage::Rust => mask_c_like(
            text,
            CLikeMaskOptions {
                rust_raw_strings: true,
                ..CLikeMaskOptions::default()
            },
        ),
        QuickCheckLanguage::JavaScriptLike | QuickCheckLanguage::JsxLike => mask_c_like(
            text,
            CLikeMaskOptions {
                single_quote_strings: true,
                template_literals: true,
                regex_literals: true,
                ..CLikeMaskOptions::default()
            },
        ),
        QuickCheckLanguage::CLike
        | QuickCheckLanguage::Go
        | QuickCheckLanguage::JavaLike
        | QuickCheckLanguage::CSharp
        | QuickCheckLanguage::GDScript
        | QuickCheckLanguage::CssLike => mask_c_like(text, CLikeMaskOptions::default()),
        QuickCheckLanguage::Python => mask_python(text),
        QuickCheckLanguage::Json => mask_json_like(text, false),
        QuickCheckLanguage::Toml | QuickCheckLanguage::Yaml | QuickCheckLanguage::ShellLike => {
            mask_json_like(text, true)
        }
        QuickCheckLanguage::HtmlXml => mask_html_xml(text),
        QuickCheckLanguage::Markdown => mask_markdown(text),
        QuickCheckLanguage::Unknown => text.to_string(),
    }
}

#[derive(Debug, Clone, Copy)]
struct CLikeMaskOptions {
    single_quote_strings: bool,
    rust_raw_strings: bool,
    template_literals: bool,
    regex_literals: bool,
}

impl Default for CLikeMaskOptions {
    fn default() -> Self {
        Self {
            single_quote_strings: false,
            rust_raw_strings: false,
            template_literals: false,
            regex_literals: false,
        }
    }
}

fn mask_c_like(text: &str, options: CLikeMaskOptions) -> String {
    let mut output = String::with_capacity(text.len());
    let mut in_string: Option<char> = None;
    let mut in_template_literal = false;
    let mut in_regex_literal = false;
    let mut escaped = false;
    let mut in_line_comment = false;
    let mut block_comment_depth = 0usize;
    let mut in_raw_string_hashes: Option<usize> = None;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\n' {
            output.push('\n');
            in_line_comment = false;
            escaped = false;
            continue;
        }
        if let Some(hashes) = in_raw_string_hashes {
            if ch == '"' && consume_raw_string_hashes(&mut chars, hashes) {
                for _ in 0..hashes {
                    output.push(' ');
                }
                in_raw_string_hashes = None;
            }
            output.push(' ');
            continue;
        }
        if in_template_literal {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '`' {
                in_template_literal = false;
            }
            output.push(' ');
            continue;
        }
        if in_regex_literal {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '/' {
                in_regex_literal = false;
            }
            output.push(' ');
            continue;
        }
        if in_line_comment {
            output.push(' ');
            continue;
        }
        if block_comment_depth > 0 {
            if ch == '/' && chars.peek() == Some(&'*') {
                output.push(' ');
                output.push(' ');
                let _ = chars.next();
                block_comment_depth += 1;
            } else if ch == '*' && chars.peek() == Some(&'/') {
                output.push(' ');
                output.push(' ');
                let _ = chars.next();
                block_comment_depth -= 1;
            } else {
                output.push(' ');
            }
            continue;
        }
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = None;
            }
            output.push(' ');
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            output.push(' ');
            output.push(' ');
            let _ = chars.next();
            in_line_comment = true;
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            output.push(' ');
            output.push(' ');
            let _ = chars.next();
            block_comment_depth = 1;
            continue;
        }
        if options.rust_raw_strings && ch == 'r' {
            if let Some(hashes) = consume_raw_string_start(&mut chars) {
                output.push(' ');
                for _ in 0..=hashes {
                    output.push(' ');
                }
                in_raw_string_hashes = Some(hashes);
                continue;
            }
        }
        if options.template_literals && ch == '`' {
            output.push(' ');
            in_template_literal = true;
            continue;
        }
        if options.regex_literals && ch == '/' && is_likely_regex_literal(&output) {
            output.push(' ');
            in_regex_literal = true;
            continue;
        }
        if ch == '"'
            || (ch == '\'' && (options.single_quote_strings || is_likely_char_literal(&chars)))
        {
            output.push(' ');
            in_string = Some(ch);
            continue;
        }
        output.push(ch);
    }

    output
}

fn mask_python(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string: Option<char> = None;
    let mut triple_quote: Option<char> = None;
    let mut escaped = false;
    let mut in_comment = false;

    while let Some(ch) = chars.next() {
        if ch == '\n' {
            output.push('\n');
            in_comment = false;
            escaped = false;
            continue;
        }
        if in_comment {
            output.push(' ');
            continue;
        }
        if let Some(quote) = triple_quote {
            if ch == quote && chars.peek() == Some(&quote) {
                let mut clone = chars.clone();
                let _ = clone.next();
                if clone.peek() == Some(&quote) {
                    output.push(' ');
                    output.push(' ');
                    output.push(' ');
                    let _ = chars.next();
                    let _ = chars.next();
                    triple_quote = None;
                    continue;
                }
            }
            output.push(' ');
            continue;
        }
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = None;
            }
            output.push(' ');
            continue;
        }
        if ch == '#' {
            output.push(' ');
            in_comment = true;
            continue;
        }
        if ch == '"' || ch == '\'' {
            if chars.peek() == Some(&ch) {
                let mut clone = chars.clone();
                let _ = clone.next();
                if clone.peek() == Some(&ch) {
                    output.push(' ');
                    output.push(' ');
                    output.push(' ');
                    let _ = chars.next();
                    let _ = chars.next();
                    triple_quote = Some(ch);
                    continue;
                }
            }
            output.push(' ');
            in_string = Some(ch);
            continue;
        }
        output.push(ch);
    }

    output
}

fn mask_json_like(text: &str, hash_comments: bool) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    let mut in_comment = false;

    while let Some(ch) = chars.next() {
        if ch == '\n' {
            output.push('\n');
            in_comment = false;
            escaped = false;
            continue;
        }
        if in_comment {
            output.push(' ');
            continue;
        }
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = None;
            }
            output.push(' ');
            continue;
        }
        if hash_comments && ch == '#' {
            output.push(' ');
            in_comment = true;
            continue;
        }
        if ch == '"' || (hash_comments && ch == '\'') {
            output.push(' ');
            in_string = Some(ch);
            continue;
        }
        output.push(ch);
    }

    output
}

fn mask_html_xml(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_comment = false;
    let mut in_quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        if ch == '\n' {
            output.push('\n');
            continue;
        }
        if in_comment {
            output.push(' ');
            if ch == '-' && chars.peek() == Some(&'-') {
                let mut clone = chars.clone();
                let _ = clone.next();
                if clone.peek() == Some(&'>') {
                    output.push(' ');
                    output.push(' ');
                    let _ = chars.next();
                    let _ = chars.next();
                    in_comment = false;
                }
            }
            continue;
        }
        if let Some(quote) = in_quote {
            if ch == quote {
                in_quote = None;
            }
            output.push(' ');
            continue;
        }
        if ch == '<' && chars.peek() == Some(&'!') {
            let mut clone = chars.clone();
            let _ = clone.next();
            if clone.next() == Some('-') && clone.next() == Some('-') {
                output.push(' ');
                output.push(' ');
                output.push(' ');
                output.push(' ');
                let _ = chars.next();
                let _ = chars.next();
                let _ = chars.next();
                in_comment = true;
                continue;
            }
        }
        if ch == '"' || ch == '\'' {
            output.push(' ');
            in_quote = Some(ch);
            continue;
        }
        output.push(ch);
    }

    output
}

fn mask_markdown(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence {
            in_fence = !in_fence;
            output.push_str(&" ".repeat(line.len()));
            output.push('\n');
            continue;
        }
        if in_fence {
            output.push_str(&" ".repeat(line.len()));
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    if !text.ends_with('\n') {
        let _ = output.pop();
    }
    output
}

fn is_likely_regex_literal(output: &str) -> bool {
    let previous = output.chars().rev().find(|ch| !ch.is_whitespace());
    matches!(
        previous,
        None | Some('(' | '[' | '{' | '=' | ':' | ',' | ';' | '!' | '&' | '|' | '?' | '+' | '-')
    )
}

fn is_likely_char_literal<I>(chars: &std::iter::Peekable<I>) -> bool
where
    I: Iterator<Item = char> + Clone,
{
    let mut clone = chars.clone();
    let Some(first) = clone.peek().copied() else {
        return false;
    };
    if first == '\n' || first.is_ascii_alphabetic() || first == '_' {
        return false;
    }
    let mut escaped = false;
    let mut body_chars = 0usize;
    while let Some(ch) = clone.next() {
        if ch == '\n' {
            return false;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '\'' {
            return body_chars > 0 && body_chars <= 8;
        }
        body_chars += 1;
        if body_chars > 8 {
            return false;
        }
    }
    false
}

fn consume_raw_string_start<I>(chars: &mut std::iter::Peekable<I>) -> Option<usize>
where
    I: Iterator<Item = char> + Clone,
{
    let mut clone = chars.clone();
    let mut hashes = 0usize;
    while clone.peek() == Some(&'#') {
        let _ = clone.next();
        hashes += 1;
    }
    if clone.next() != Some('"') {
        return None;
    }
    for _ in 0..hashes {
        let _ = chars.next();
    }
    let _ = chars.next();
    Some(hashes)
}

fn consume_raw_string_hashes<I>(chars: &mut std::iter::Peekable<I>, hashes: usize) -> bool
where
    I: Iterator<Item = char> + Clone,
{
    let mut clone = chars.clone();
    for _ in 0..hashes {
        if clone.next() != Some('#') {
            return false;
        }
    }
    for _ in 0..hashes {
        let _ = chars.next();
    }
    true
}

fn matches_delimiter(open: char, close: char) -> bool {
    matches!((open, close), ('{', '}') | ('[', ']') | ('(', ')'))
}

fn language_for_path(path: &Path) -> QuickCheckLanguage {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if file_name.starts_with(".env") {
        return QuickCheckLanguage::ShellLike;
    }
    match file_name.as_str() {
        "dockerfile" | "makefile" => return QuickCheckLanguage::ShellLike,
        ".gitignore" | ".gitattributes" | ".editorconfig" => {
            return QuickCheckLanguage::ShellLike;
        }
        _ => {}
    }

    lowercase_extension(path)
        .map(|extension| match extension.as_str() {
            "rs" => QuickCheckLanguage::Rust,
            "py" => QuickCheckLanguage::Python,
            "js" | "ts" => QuickCheckLanguage::JavaScriptLike,
            "jsx" | "tsx" => QuickCheckLanguage::JsxLike,
            "c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" => QuickCheckLanguage::CLike,
            "go" => QuickCheckLanguage::Go,
            "java" => QuickCheckLanguage::JavaLike,
            "cs" => QuickCheckLanguage::CSharp,
            "gd" => QuickCheckLanguage::GDScript,
            "json" => QuickCheckLanguage::Json,
            "toml" => QuickCheckLanguage::Toml,
            "yaml" | "yml" => QuickCheckLanguage::Yaml,
            "css" | "scss" => QuickCheckLanguage::CssLike,
            "html" | "xml" => QuickCheckLanguage::HtmlXml,
            "md" | "mdx" => QuickCheckLanguage::Markdown,
            _ => QuickCheckLanguage::Unknown,
        })
        .unwrap_or(QuickCheckLanguage::Unknown)
}

pub fn apply_patch_succeeded(tool_response: Option<&serde_json::Value>) -> bool {
    let Some(response) = tool_response else {
        return false;
    };
    let response_text = response.to_string().to_ascii_lowercase();
    ![
        "error",
        "failed",
        "invalid context",
        "patch failed",
        "did not apply",
        "exception",
    ]
    .iter()
    .any(|needle| response_text.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_add_update_delete_and_move_paths() {
        let patch = r#"*** Begin Patch
*** Add File: src/new.rs
*** Update File: src/lib.rs
*** Delete File: src/old.rs
*** Update File: src/old.rs
*** Move to: src/moved.rs
*** End Patch"#;

        let paths = extract_apply_patch_paths(patch, Some("C:\\repo")).unwrap();

        assert_eq!(
            paths,
            vec![
                PathBuf::from("C:\\repo").join("src/new.rs"),
                PathBuf::from("C:\\repo").join("src/lib.rs"),
                PathBuf::from("C:\\repo").join("src/moved.rs"),
            ]
        );
    }

    #[test]
    fn duplicate_paths_collapse() {
        let patch = "*** Update File: src/lib.rs\n*** Update File: src/lib.rs\n";

        let paths = extract_apply_patch_paths(patch, Some("C:\\repo")).unwrap();

        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn clean_code_produces_no_report() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("lib.rs");
        fs::write(&file, "fn main() {\n    println!(\"ok\");\n}\n").unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, Some("C:\\fallback"));

        assert!(report.is_none());
    }

    #[test]
    fn language_mapping_covers_all_quick_check_paths() {
        let cases = [
            ("lib.rs", QuickCheckLanguage::Rust),
            ("main.py", QuickCheckLanguage::Python),
            ("app.js", QuickCheckLanguage::JavaScriptLike),
            ("app.jsx", QuickCheckLanguage::JsxLike),
            ("app.ts", QuickCheckLanguage::JavaScriptLike),
            ("app.tsx", QuickCheckLanguage::JsxLike),
            ("main.c", QuickCheckLanguage::CLike),
            ("main.cc", QuickCheckLanguage::CLike),
            ("main.cpp", QuickCheckLanguage::CLike),
            ("main.cxx", QuickCheckLanguage::CLike),
            ("main.h", QuickCheckLanguage::CLike),
            ("main.hh", QuickCheckLanguage::CLike),
            ("main.hpp", QuickCheckLanguage::CLike),
            ("main.hxx", QuickCheckLanguage::CLike),
            ("main.cs", QuickCheckLanguage::CSharp),
            ("Main.java", QuickCheckLanguage::JavaLike),
            ("main.go", QuickCheckLanguage::Go),
            ("node.gd", QuickCheckLanguage::GDScript),
            ("data.json", QuickCheckLanguage::Json),
            ("Cargo.toml", QuickCheckLanguage::Toml),
            ("config.yaml", QuickCheckLanguage::Yaml),
            ("config.yml", QuickCheckLanguage::Yaml),
            ("README.md", QuickCheckLanguage::Markdown),
            ("README.mdx", QuickCheckLanguage::Markdown),
            ("style.css", QuickCheckLanguage::CssLike),
            ("style.scss", QuickCheckLanguage::CssLike),
            ("index.html", QuickCheckLanguage::HtmlXml),
            ("layout.xml", QuickCheckLanguage::HtmlXml),
            ("Dockerfile", QuickCheckLanguage::ShellLike),
            ("Makefile", QuickCheckLanguage::ShellLike),
            (".env.local", QuickCheckLanguage::ShellLike),
            (".gitignore", QuickCheckLanguage::ShellLike),
            (".gitattributes", QuickCheckLanguage::ShellLike),
            (".editorconfig", QuickCheckLanguage::ShellLike),
        ];

        for (path, language) in cases {
            assert_eq!(language_for_path(Path::new(path)), language, "{path}");
        }
        assert_eq!(
            language_for_path(Path::new("note.txt")),
            QuickCheckLanguage::Unknown
        );
    }

    #[test]
    fn conflict_marker_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("lib.rs");
        fs::write(&file, "fn main() {\n<<<<<<< ours\n}\n").unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None).unwrap();

        assert!(report.render().contains("git conflict marker"));
    }

    #[test]
    fn patch_marker_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("lib.rs");
        fs::write(&file, "fn main() {}\n*** Begin Patch\n").unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None).unwrap();

        assert!(report.render().contains("patch marker left in file"));
    }

    #[test]
    fn missing_binary_and_non_code_files_fail_open() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.rs");
        let binary = temp.path().join("data.rs");
        let text = temp.path().join("note.bin");
        fs::write(&binary, b"abc\0def").unwrap();
        fs::write(&text, "<<<<<<< not checked\n").unwrap();
        let patch = format!(
            "*** Update File: {}\n*** Update File: {}\n*** Update File: {}",
            missing.display(),
            binary.display(),
            text.display()
        );

        let report = check_apply_patch_command(&patch, None);

        assert!(report.is_none());
    }

    #[test]
    fn unmatched_delimiter_is_reported_for_brace_language() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("lib.rs");
        fs::write(&file, "fn main() {\n").unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None).unwrap();

        assert!(report.render().contains("unclosed delimiter"));
    }

    #[test]
    fn delimiters_inside_comments_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("lib.rs");
        fs::write(&file, "fn main() {}\n/* { [ ( */\n// } ] )\n").unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None);

        assert!(report.is_none());
    }

    #[test]
    fn c_like_literals_preprocessor_and_comments_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("main.cpp");
        fs::write(
            &file,
            "#define TEXT \"{ [ ( <<<<<<< *** Begin Patch\"\nint main() { char c = '}'; /* { [ ( */ return 0; }\n",
        )
        .unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None);

        assert!(report.is_none());
    }

    #[test]
    fn js_ts_templates_regex_and_jsx_text_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("view.tsx");
        fs::write(
            &file,
            "const text = `<<<<<<< { [ (`;\nconst re = /}\\]\\)/;\nexport const View = () => <div data-x=\"{ [ (\">{'*** Begin Patch'}</div>;\n",
        )
        .unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None);

        assert!(report.is_none());
    }

    #[test]
    fn python_triple_strings_and_comments_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("script.py");
        fs::write(
            &file,
            "def main():\n    text = '''<<<<<<< { [ ( *** Begin Patch'''\n    value = 1  # } ] ) TODO_REMOVE\n",
        )
        .unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None);

        assert!(report.is_none());
    }

    #[test]
    fn config_strings_with_markers_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        for (name, content) in [
            ("data.json", "{\"value\":\"<<<<<<< { [ (\"}\n"),
            ("config.toml", "value = '*** Begin Patch { [ ('\n"),
            ("config.yaml", "value: \"TODO_REMOVE { [ (\"\n"),
        ] {
            let file = temp.path().join(name);
            fs::write(&file, content).unwrap();
            let patch = format!("*** Update File: {}", file.display());

            let report = check_apply_patch_command(&patch, None);

            assert!(report.is_none(), "{name}");
        }
    }

    #[test]
    fn markdown_fenced_blocks_are_marker_only_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("README.md");
        fs::write(
            &file,
            "# Notes\n```rust\n<<<<<<< ours\nfn main() {\n*** Begin Patch\n```\n",
        )
        .unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None);

        assert!(report.is_none());
    }

    #[test]
    fn html_xml_comments_and_attributes_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("index.html");
        fs::write(
            &file,
            "<!-- <<<<<<< { [ ( -->\n<div data-value=\"*** Begin Patch { [ (\"></div>\n",
        )
        .unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None);

        assert!(report.is_none());
    }

    #[test]
    fn shell_like_files_are_marker_only() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("Dockerfile");
        fs::write(&file, "RUN echo '{ [ ('\nENV FLAG=TODO_REMOVE\n").unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None).unwrap();

        assert!(report.render().contains("remove placeholder"));
        assert!(!report.render().contains("delimiter"));
    }

    #[test]
    fn delimiter_imbalance_reports_for_brace_families() {
        let temp = tempfile::tempdir().unwrap();
        for name in [
            "lib.rs",
            "main.cpp",
            "app.ts",
            "main.go",
            "Main.java",
            "Program.cs",
            "node.gd",
            "style.css",
            "data.json",
        ] {
            let file = temp.path().join(name);
            fs::write(&file, "{\n").unwrap();
            let patch = format!("*** Update File: {}", file.display());

            let Some(report) = check_apply_patch_command(&patch, None) else {
                panic!("{name}: expected syntax warning");
            };

            assert!(report.render().contains("unclosed delimiter"), "{name}");
        }
    }

    #[test]
    fn true_markers_outside_masked_regions_are_reported() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["lib.rs", "main.cpp", "app.ts", "script.py", "README.md"] {
            let file = temp.path().join(name);
            fs::write(&file, "<<<<<<< ours\n").unwrap();
            let patch = format!("*** Update File: {}", file.display());

            let Some(report) = check_apply_patch_command(&patch, None) else {
                panic!("{name}: expected syntax warning");
            };

            assert!(report.render().contains("git conflict marker"), "{name}");
        }
    }

    #[test]
    fn syntax_parse_warnings_report_builtin_parser_languages() {
        let temp = tempfile::tempdir().unwrap();
        for (name, content) in [
            ("bad.rs", "fn main() { let = 1; }\n"),
            ("bad.py", "def main():\n    return = 1\n"),
            ("bad.c", "int main(void) { int = 1; return 0; }\n"),
            ("bad.cpp", "class Demo { public: void f() { int = 1; } };\n"),
            ("bad.gd", "func _ready():\n    if :\n        pass\n"),
        ] {
            let file = temp.path().join(name);
            fs::write(&file, content).unwrap();
            let patch = format!("*** Update File: {}", file.display());

            let Some(report) = check_apply_patch_command(&patch, None) else {
                panic!("{name}: expected syntax warning");
            };
            let rendered = report.render();

            assert!(rendered.contains("warning"), "{name}: {rendered}");
            assert!(
                rendered.contains("syntax parse issue detected"),
                "{name}: {rendered}"
            );
        }
    }

    #[test]
    fn valid_builtin_parser_languages_do_not_report_syntax_warnings() {
        let temp = tempfile::tempdir().unwrap();
        for (name, content) in [
            ("ok.rs", "fn main() { let _x = 1; }\n"),
            ("ok.py", "def main():\n    return 1\n"),
            ("ok.c", "int main(void) { return 0; }\n"),
            ("ok.cpp", "class Demo { public: void f() {} };\n"),
            ("ok.gd", "func _ready():\n    pass\n"),
        ] {
            let file = temp.path().join(name);
            fs::write(&file, content).unwrap();
            let patch = format!("*** Update File: {}", file.display());

            let report = check_apply_patch_command(&patch, None);

            assert!(report.is_none(), "{name}: {report:?}");
        }
    }

    #[test]
    fn delimiter_errors_skip_duplicate_syntax_warnings() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("lib.rs");
        fs::write(&file, "fn main() {\n").unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None).unwrap();
        let rendered = report.render();

        assert!(rendered.contains("unclosed delimiter"));
        assert!(!rendered.contains("syntax parse issue detected"));
    }

    #[test]
    fn markers_and_debug_macros_inside_strings_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("lib.rs");
        fs::write(
            &file,
            r#"fn main() {
    let _ = "<<<<<<< ours *** Begin Patch TODO_REMOVE dbg!(x) todo!() unimplemented!()";
}
"#,
        )
        .unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None);

        assert!(report.is_none());
    }

    #[test]
    fn rust_lifetimes_do_not_poison_delimiter_scan() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("lib.rs");
        fs::write(
            &file,
            r#"fn operation_kind() -> &'static str {
    assert!(output.contains("\"decision\":\"block\""));
    "ok"
}
"#,
        )
        .unwrap();
        let patch = format!("*** Update File: {}", file.display());

        let report = check_apply_patch_command(&patch, None);

        assert!(report.is_none());
    }

    #[test]
    fn checker_source_strings_do_not_report_own_rules() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("integrations")
            .join("post_edit_check.rs");

        let findings = check_file(&path).unwrap();

        assert!(
            findings.is_empty(),
            "unexpected self findings: {:?}",
            findings
        );
    }

    #[test]
    fn failed_tool_response_is_not_successful() {
        assert!(apply_patch_succeeded(Some(&json!({"output":"Done!"}))));
        assert!(!apply_patch_succeeded(Some(
            &json!({"error":"patch failed"})
        )));
        assert!(!apply_patch_succeeded(None));
    }
}
