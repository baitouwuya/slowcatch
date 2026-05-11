use anyhow::{Result, bail};
use std::fs;
use std::path::Path;
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInspection {
    pub file_path: String,
    pub file_kind: &'static str,
    pub render: &'static str,
    pub bytes: u64,
    pub lines: usize,
    pub body: String,
}

pub fn inspect_file(path: &Path, raw: bool) -> Result<String> {
    Ok(inspect_file_structured(path, raw)?.render())
}

pub fn inspect_file_structured(path: &Path, _raw: bool) -> Result<FileInspection> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        bail!("not a file: {}", path.display());
    }
    let bytes = fs::read(path)?;
    if bytes.contains(&0) {
        bail!("binary file: {}", path.display());
    }
    let text = String::from_utf8(bytes)?;
    let lines: Vec<&str> = text.lines().collect();
    let kind = FileKind::from_path_and_text(path, &lines);
    let small = is_small_file(kind, metadata.len(), lines.len());

    let render = if small {
        "full"
    } else if kind == FileKind::Code {
        "outline"
    } else if kind == FileKind::Unknown {
        "preview"
    } else {
        "summary"
    };

    let mut body = Vec::new();
    if render == "full" {
        for (index, line) in lines.iter().enumerate() {
            body.push(format!("L{} | {}", index + 1, line));
        }
    } else {
        body.push("hint=use slice/read_lines for exact ranges".to_string());
        match kind {
            FileKind::Code => {
                if lines.len() > 500 {
                    body.push("advice=large_code_prefer_split_modules".to_string());
                }
                let symbols = code_outline(path, &text);
                if symbols.is_empty() {
                    body.extend(non_empty_preview(&lines));
                } else {
                    body.extend(symbols);
                }
            }
            FileKind::Config | FileKind::Lockfile => body.extend(config_summary(&lines)),
            FileKind::Markdown => body.extend(markdown_summary(&lines)),
            FileKind::Text | FileKind::Unknown => body.extend(non_empty_preview(&lines)),
        }
    }

    Ok(FileInspection {
        file_path: path.display().to_string(),
        file_kind: kind.as_str(),
        render,
        bytes: metadata.len(),
        lines: lines.len(),
        body: body.join("\n"),
    })
}

impl FileInspection {
    pub fn header_lines(&self) -> Vec<String> {
        vec![
            format!("file_path={}", self.file_path),
            format!("file_kind={}", self.file_kind),
            format!("render={}", self.render),
            format!(
                "bytes={} lines={} line_numbers=original",
                self.bytes, self.lines
            ),
            "line_format=source:L<n>| summary:L<n> <type>|".to_string(),
        ]
    }

    pub fn render(&self) -> String {
        let mut output = self.header_lines();
        if !self.body.is_empty() {
            output.push(String::new());
            output.push(self.body.clone());
        }
        output.join("\n")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Code,
    Config,
    Lockfile,
    Markdown,
    Text,
    Unknown,
}

impl FileKind {
    fn from_path_and_text(path: &Path, lines: &[&str]) -> Self {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if is_lockfile_name(&file_name) {
            return Self::Lockfile;
        }
        if is_config_name(&file_name) {
            return Self::Config;
        }

        let by_extension = match path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "rs" | "py" | "c" | "cc" | "cpp" | "h" | "hpp" | "gd" => Self::Code,
            "json" | "toml" | "yaml" | "yml" | "tres" | "cfg" | "ini" => Self::Config,
            "md" | "mdx" | "txt" => Self::Markdown,
            _ => Self::Unknown,
        };
        if by_extension != Self::Unknown {
            return by_extension;
        }

        if looks_like_config(lines) {
            Self::Config
        } else if looks_like_text(lines) {
            Self::Text
        } else {
            Self::Unknown
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Config => "config",
            Self::Lockfile => "lockfile",
            Self::Markdown => "markdown",
            Self::Text => "text",
            Self::Unknown => "unknown",
        }
    }
}

fn is_lockfile_name(name: &str) -> bool {
    matches!(
        name,
        "cargo.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "bun.lockb"
    ) || name.ends_with(".lock")
}

fn is_config_name(name: &str) -> bool {
    name.starts_with(".env")
        || matches!(
            name,
            "dockerfile"
                | "makefile"
                | ".gitignore"
                | ".gitattributes"
                | ".editorconfig"
                | ".npmrc"
                | ".yarnrc"
                | ".dockerignore"
        )
}

fn looks_like_config(lines: &[&str]) -> bool {
    let mut meaningful = 0usize;
    let mut config_like = 0usize;
    for line in lines.iter().take(50) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        meaningful += 1;
        if trimmed.starts_with('[') && trimmed.ends_with(']') || is_config_key_line(trimmed) {
            config_like += 1;
        }
    }
    meaningful > 0 && config_like * 2 >= meaningful
}

fn looks_like_text(lines: &[&str]) -> bool {
    lines.iter().take(20).any(|line| !line.trim().is_empty())
}

fn is_small_file(kind: FileKind, bytes: u64, lines: usize) -> bool {
    match kind {
        FileKind::Code => bytes <= 12 * 1024 && lines <= 300,
        FileKind::Config | FileKind::Lockfile | FileKind::Markdown => {
            bytes <= 64 * 1024 && lines <= 2_000
        }
        FileKind::Text => bytes <= 16 * 1024 && lines <= 400,
        FileKind::Unknown => bytes <= 16 * 1024 && lines <= 400,
    }
}

fn code_outline(path: &Path, text: &str) -> Vec<String> {
    tree_sitter_outline(path, text).unwrap_or_else(|| regex_outline(text))
}

fn tree_sitter_outline(path: &Path, text: &str) -> Option<Vec<String>> {
    let language = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "cc" | "cpp" | "hpp" => tree_sitter_cpp::LANGUAGE.into(),
        "gd" => tree_sitter_gdscript::LANGUAGE.into(),
        _ => return None,
    };
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(text, None)?;
    let mut output = Vec::new();
    collect_symbols(tree.root_node(), text, &mut output);
    Some(output)
}

fn collect_symbols(node: Node<'_>, text: &str, output: &mut Vec<String>) {
    if let Some(label) = symbol_label(node.kind(), node, text) {
        let start = node.start_position().row + 1;
        let end = node.end_position().row + 1;
        if start == end {
            output.push(format!("L{start} symbol | {label}"));
        } else {
            output.push(format!("L{start} symbol | {label} range=L{start}-L{end}"));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_symbols(child, text, output);
    }
}

fn symbol_label(kind: &str, node: Node<'_>, text: &str) -> Option<String> {
    let category = match kind {
        "function_item"
        | "function_definition"
        | "function_declaration"
        | "method_definition"
        | "function_declarator"
        | "function_statement" => "function",
        "class_definition" | "class_declaration" => "class",
        "struct_item" | "struct_specifier" => "struct",
        "enum_item" => "enum",
        "interface_declaration" => "interface",
        "const_item" | "static_item" => "constant",
        "let_declaration" | "assignment_statement" => "variable",
        _ => return None,
    };
    let name = named_child_text(node, text).unwrap_or_else(|| first_line(node, text));
    Some(format!("{category} {}", compact(&name)))
}

fn named_child_text(node: Node<'_>, text: &str) -> Option<String> {
    for field in ["name", "declarator"] {
        if let Some(child) = node.child_by_field_name(field) {
            return child.utf8_text(text.as_bytes()).ok().map(ToOwned::to_owned);
        }
    }
    None
}

fn first_line(node: Node<'_>, text: &str) -> String {
    node.utf8_text(text.as_bytes())
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

fn regex_outline(text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim_start();
            let label = if trimmed.starts_with("fn ")
                || trimmed.starts_with("def ")
                || trimmed.starts_with("func ")
                || trimmed.contains(" function ")
            {
                Some("function")
            } else if trimmed.starts_with("class ") {
                Some("class")
            } else if trimmed.starts_with("struct ") {
                Some("struct")
            } else if trimmed.starts_with("const ") || trimmed.starts_with("static ") {
                Some("constant")
            } else {
                None
            }?;
            Some(format!(
                "L{} symbol | {} {}",
                index + 1,
                label,
                compact(trimmed)
            ))
        })
        .collect()
}

fn config_summary(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                Some(format!("L{} section | {}", index + 1, trimmed))
            } else if is_config_key_line(trimmed) {
                Some(format!("L{} key | {}", index + 1, config_key(trimmed)))
            } else {
                None
            }
        })
        .take(200)
        .collect()
}

fn is_config_key_line(line: &str) -> bool {
    !line.starts_with('#') && !line.starts_with("//") && (line.contains('=') || line.contains(':'))
}

fn config_key(line: &str) -> String {
    line.split(['=', ':'])
        .next()
        .unwrap_or(line)
        .trim_matches('"')
        .trim()
        .to_string()
}

fn markdown_summary(lines: &[&str]) -> Vec<String> {
    let mut output = Vec::new();
    let mut in_fence = false;
    let mut fence_start = 0;
    let mut fence_lang = String::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_fence {
                let lang = if fence_lang.is_empty() {
                    String::new()
                } else {
                    format!(" lang={fence_lang}")
                };
                output.push(format!(
                    "L{fence_start} fence | range=L{fence_start}-L{}{}",
                    index + 1,
                    lang
                ));
                in_fence = false;
                fence_lang.clear();
            } else {
                fence_start = index + 1;
                fence_lang = trimmed.trim_start_matches("```").trim().to_string();
                in_fence = true;
            }
        }
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|value| *value == '#').count();
            output.push(format!(
                "L{} heading | h{} {}",
                index + 1,
                level,
                trimmed[level..].trim()
            ));
        }
    }
    output
}

fn non_empty_preview(lines: &[&str]) -> Vec<String> {
    let mut output: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(5)
        .map(|(index, line)| format!("L{} preview | {}", index + 1, compact(line)))
        .collect();
    if output.is_empty() {
        output.push("preview=<empty>".to_string());
    }
    output
}

fn compact(value: &str) -> String {
    const MAX: usize = 180;
    let value = value.trim().replace('\t', " ");
    if value.chars().count() > MAX {
        format!("{}...", value.chars().take(MAX).collect::<String>())
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn small_code_file_outputs_numbered_lines() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.rs");
        fs::write(&file, "fn main() {\n    println!(\"hi\");\n}\n").unwrap();

        let output = inspect_file(&file, false).unwrap();

        assert!(output.contains("render=full"));
        assert!(output.contains("file_kind=code"));
        assert!(output.contains("line_format=source:L<n>| summary:L<n> <type>|"));
        assert!(output.contains("L1 | fn main() {"));
        assert!(output.contains("L2 |     println!"));
    }

    #[test]
    fn large_code_file_outputs_outline_and_large_file_advice() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("large.py");
        let mut text = String::new();
        text.push_str("import os\nclass Runner:\n");
        for index in 0..520 {
            text.push_str(&format!(
                "    def method_{index}(self):\n        return {index}\n"
            ));
        }
        fs::write(&file, text).unwrap();

        let output = inspect_file(&file, false).unwrap();

        assert!(output.contains("render=outline"));
        assert!(output.contains("L2 symbol | class Runner"));
        assert!(output.contains("L3 symbol | function method_0"));
        assert!(output.contains("advice=large_code_prefer_split_modules"));
        assert!(!output.contains("return 0"));
    }

    #[test]
    fn markdown_large_file_outputs_heading_summary() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("doc.md");
        let mut text = String::new();
        for index in 0..2100 {
            if index % 500 == 0 {
                text.push_str(&format!("# Heading {index}\n"));
            } else {
                text.push_str("body\n");
            }
        }
        fs::write(&file, text).unwrap();

        let output = inspect_file(&file, false).unwrap();

        assert!(output.contains("render=summary"));
        assert!(output.contains("L1 heading | h1 Heading 0"));
        assert!(output.contains("hint=use slice/read_lines for exact ranges"));
    }

    #[test]
    fn markdown_fence_uses_typed_range_record() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("doc.md");
        let mut text = String::new();
        text.push_str("# Title\n");
        text.push_str("body\n");
        text.push_str("```powershell\n");
        text.push_str("cargo test\n");
        text.push_str("```\n");
        for _ in 0..2100 {
            text.push_str("body\n");
        }
        fs::write(&file, text).unwrap();

        let output = inspect_file(&file, false).unwrap();

        assert!(output.contains("L3 fence | range=L3-L5 lang=powershell"));
        assert!(!output.contains("L3-L5 fenced_code"));
    }

    #[test]
    fn large_config_outputs_typed_keys_and_sections() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("large.toml");
        let mut text = String::new();
        text.push_str("[package]\nname = \"demo\"\n");
        for index in 0..2100 {
            text.push_str(&format!("key{index} = \"value\"\n"));
        }
        fs::write(&file, text).unwrap();

        let output = inspect_file(&file, false).unwrap();

        assert!(output.contains("file_kind=config"));
        assert!(output.contains("L1 section | [package]"));
        assert!(output.contains("L2 key | name"));
    }

    #[test]
    fn cargo_lock_is_recognized_as_lockfile() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("Cargo.lock");
        let mut text = String::new();
        text.push_str("# generated\nversion = 4\n[[package]]\nname = \"demo\"\n");
        for index in 0..2100 {
            text.push_str(&format!("checksum{index} = \"abc\"\n"));
        }
        fs::write(&file, text).unwrap();

        let output = inspect_file(&file, false).unwrap();

        assert!(output.contains("file_kind=lockfile"));
        assert!(output.contains("L2 key | version"));
        assert!(output.contains("L3 section | [[package]]"));
    }

    #[test]
    fn env_dockerfile_and_extensionless_config_classify_sensibly() {
        let temp = tempfile::tempdir().unwrap();
        let env_file = temp.path().join(".env.local");
        fs::write(&env_file, "TOKEN=abc\n").unwrap();
        let dockerfile = temp.path().join("Dockerfile");
        fs::write(&dockerfile, "FROM rust:latest\n").unwrap();
        let extensionless = temp.path().join("settings");
        fs::write(&extensionless, "name = \"demo\"\nvalue = 1\n").unwrap();

        assert!(
            inspect_file(&env_file, false)
                .unwrap()
                .contains("file_kind=config")
        );
        assert!(
            inspect_file(&dockerfile, false)
                .unwrap()
                .contains("file_kind=config")
        );
        assert!(
            inspect_file(&extensionless, false)
                .unwrap()
                .contains("file_kind=config")
        );
    }

    #[test]
    fn binary_file_fails() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.bin");
        fs::write(&file, [0, 159, 146, 150]).unwrap();

        assert!(inspect_file(&file, false).is_err());
    }
}
