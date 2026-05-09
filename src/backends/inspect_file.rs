use anyhow::{Result, bail};
use std::fs;
use std::path::Path;
use tree_sitter::{Node, Parser};

pub fn inspect_file(path: &Path, raw: bool) -> Result<String> {
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
    let kind = FileKind::from_path(path);
    let small = is_small_file(kind, metadata.len(), lines.len());

    let mut output = Vec::new();
    let mode = if small {
        "full"
    } else if kind == FileKind::Code {
        "outline"
    } else {
        "summary"
    };
    output.push(format!(
        "path={} mode={} raw={} bytes={} lines={} note=Line numbers refer to the original file.",
        path.display(),
        mode,
        raw,
        metadata.len(),
        lines.len()
    ));

    if small {
        for (index, line) in lines.iter().enumerate() {
            output.push(format!("L{} | {}", index + 1, line));
        }
        return Ok(output.join("\n"));
    }

    output
        .push("Use slice for exact ranges: slowcatch slice <path> --skip N --first M".to_string());
    match kind {
        FileKind::Code => {
            if lines.len() > 500 {
                output.push(
                    "advice=不建议继续在该文件新增功能；优先拆分、组合或新增模块。".to_string(),
                );
            }
            let symbols = code_outline(path, &text);
            if symbols.is_empty() {
                output.extend(non_empty_preview(&lines));
            } else {
                output.extend(symbols);
            }
        }
        FileKind::Config => output.extend(config_summary(&lines)),
        FileKind::Markdown => output.extend(markdown_summary(&lines)),
        FileKind::Unknown => output.extend(non_empty_preview(&lines)),
    }
    Ok(output.join("\n"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Code,
    Config,
    Markdown,
    Unknown,
}

impl FileKind {
    fn from_path(path: &Path) -> Self {
        match path
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
        }
    }
}

fn is_small_file(kind: FileKind, bytes: u64, lines: usize) -> bool {
    match kind {
        FileKind::Code => bytes <= 12 * 1024 && lines <= 300,
        FileKind::Config | FileKind::Markdown => bytes <= 64 * 1024 && lines <= 2_000,
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
            output.push(format!("L{start} {label}"));
        } else {
            output.push(format!("L{start} {label} range=L{start}-L{end}"));
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
            Some(format!("L{} {} {}", index + 1, label, compact(trimmed)))
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
                Some(format!("L{} section {}", index + 1, trimmed))
            } else if is_config_key_line(trimmed) {
                Some(format!("L{} key {}", index + 1, config_key(trimmed)))
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
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_fence {
                output.push(format!("L{fence_start}-L{} fenced_code", index + 1));
                in_fence = false;
            } else {
                fence_start = index + 1;
                in_fence = true;
            }
        }
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|value| *value == '#').count();
            output.push(format!(
                "L{} heading{} {}",
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
        .map(|(index, line)| format!("L{} preview {}", index + 1, compact(line)))
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

        assert!(output.contains("mode=full"));
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

        assert!(output.contains("mode=outline"));
        assert!(output.contains("L2 class Runner"));
        assert!(output.contains("L3 function method_0"));
        assert!(output.contains("不建议继续在该文件新增功能"));
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

        assert!(output.contains("mode=summary"));
        assert!(output.contains("L1 heading1 Heading 0"));
        assert!(output.contains("Use slice for exact ranges"));
    }

    #[test]
    fn binary_file_fails() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.bin");
        fs::write(&file, [0, 159, 146, 150]).unwrap();

        assert!(inspect_file(&file, false).is_err());
    }
}
