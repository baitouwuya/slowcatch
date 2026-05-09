use crate::backends::everything::FindQuery;
use crate::backends::grep::{GrepContextQuery, GrepQuery};
use crate::backends::projection::{FindProjection, MetadataField, Projection, ReadOnlyExternal};
use regex::Regex;
use std::path::PathBuf;
use tree_sitter::Parser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FastOperation {
    Find(FindQuery),
    FindProjection(FindProjection),
    Grep(GrepQuery),
    GrepContext(GrepContextQuery),
    InspectFile {
        path: PathBuf,
        raw: bool,
    },
    Slice {
        path: PathBuf,
        skip: usize,
        first: usize,
    },
    CommandList(Vec<FastSegment>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FastSegment {
    Operation(Box<FastOperation>),
    ReadOnlyExternal(ReadOnlyExternal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseDecision {
    Fast(FastOperation),
    PassThrough(PassThroughReason),
    UnknownCandidate(UnknownCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassThroughReason {
    DynamicOrUnsafe,
    Mutating,
    ExternalProgram,
    InvalidPowerShell,
    NotAFileCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCommand {
    pub reason: String,
    pub first_command: Option<String>,
    pub pipeline_commands: Vec<String>,
    pub normalized_shape: String,
}

pub fn parse_powershell(command: &str, cwd: Option<&str>) -> Option<FastOperation> {
    match classify_powershell(command, cwd) {
        ParseDecision::Fast(operation) => Some(operation),
        ParseDecision::PassThrough(_) | ParseDecision::UnknownCandidate(_) => None,
    }
}

pub fn classify_powershell(command: &str, cwd: Option<&str>) -> ParseDecision {
    let trimmed = unwrap_shell(command.trim());
    if trimmed.is_empty() {
        return ParseDecision::PassThrough(PassThroughReason::NotAFileCommand);
    }

    if let Some(operation) = parse_command_list(trimmed, cwd) {
        return ParseDecision::Fast(operation);
    }

    let tokens = tokenize(trimmed).unwrap_or_default();
    let first_command = tokens.first().map(|value| normalize_command_name(value));

    if has_dynamic_or_unsafe_constructs(trimmed) {
        return ParseDecision::PassThrough(PassThroughReason::DynamicOrUnsafe);
    }
    if is_mutating_command(trimmed, first_command.as_deref()) {
        return ParseDecision::PassThrough(PassThroughReason::Mutating);
    }
    if first_command
        .as_deref()
        .is_some_and(is_external_program_command)
    {
        return ParseDecision::PassThrough(PassThroughReason::ExternalProgram);
    }
    if first_command
        .as_deref()
        .is_some_and(|command| !is_interesting_file_command(command))
    {
        return ParseDecision::PassThrough(PassThroughReason::ExternalProgram);
    }
    if let Some(operation) = parse_find_projection(trimmed, cwd) {
        return ParseDecision::Fast(operation);
    }
    if !is_valid_powershell_ast(trimmed) {
        if is_unknown_candidate(trimmed, first_command.as_deref()) {
            return unknown_candidate(trimmed, "invalid_or_unsupported_powershell_shape");
        }
        return ParseDecision::PassThrough(PassThroughReason::InvalidPowerShell);
    }

    if let Some(operation) = parse_get_content_slice(trimmed) {
        return ParseDecision::Fast(operation);
    }

    if let Some(operation) = parse_get_content_select_string(trimmed) {
        return ParseDecision::Fast(operation);
    }

    if let Some(operation) = parse_select_string(trimmed, cwd) {
        return ParseDecision::Fast(operation);
    }

    if let Some(operation) = parse_get_content_inspect(trimmed) {
        return ParseDecision::Fast(operation);
    }

    if let Some(operation) = parse_get_child_item_find(trimmed, cwd) {
        return ParseDecision::Fast(operation);
    }

    if is_unknown_candidate(trimmed, first_command.as_deref()) {
        return unknown_candidate(trimmed, "unsupported_file_command_shape");
    }

    ParseDecision::PassThrough(PassThroughReason::NotAFileCommand)
}

fn unwrap_shell(command: &str) -> &str {
    command
}

fn is_valid_powershell_ast(command: &str) -> bool {
    let mut parser = Parser::new();
    let language = tree_sitter_powershell::LANGUAGE;
    parser.set_language(&language.into()).is_ok()
        && parser
            .parse(command, None)
            .map(|tree| !tree.root_node().has_error())
            .unwrap_or(false)
}

fn has_dynamic_or_unsafe_constructs(command: &str) -> bool {
    let lowered = command.to_lowercase();
    ["$(", "`", "invoke-expression", "iex"]
        .iter()
        .any(|token| lowered.contains(token))
}

fn is_mutating_command(command: &str, first_command: Option<&str>) -> bool {
    if command.contains(">>") || command.contains('>') {
        return true;
    }
    first_command.is_some_and(|command| {
        matches!(
            command,
            "remove-item"
                | "rm"
                | "rmdir"
                | "del"
                | "erase"
                | "move-item"
                | "mv"
                | "set-content"
                | "sc"
                | "new-item"
                | "ni"
                | "copy-item"
                | "cp"
                | "copy"
                | "rename-item"
                | "ren"
                | "clear-content"
                | "clc"
                | "out-file"
        )
    })
}

fn is_external_program_command(command: &str) -> bool {
    matches!(
        command,
        "git"
            | "cargo"
            | "rg"
            | "fd"
            | "node"
            | "python"
            | "python3"
            | "py"
            | "pwsh"
            | "powershell"
            | "cmd"
            | "docker"
            | "npm"
            | "pnpm"
            | "bun"
            | "uv"
    ) || command.ends_with(".exe")
        || command.ends_with(".cmd")
        || command.ends_with(".bat")
        || command.ends_with(".ps1")
        || command.contains('\\')
        || command.contains('/')
        || command.starts_with('.')
}

fn is_interesting_file_command(command: &str) -> bool {
    matches!(
        command,
        "get-childitem"
            | "gci"
            | "dir"
            | "ls"
            | "get-content"
            | "gc"
            | "cat"
            | "type"
            | "select-string"
            | "sls"
            | "select-object"
            | "select"
            | "where-object"
            | "where"
            | "?"
            | "sort-object"
            | "sort"
            | "measure-object"
            | "measure"
    )
}

fn is_unknown_candidate(command: &str, first_command: Option<&str>) -> bool {
    if first_command.is_some_and(is_interesting_file_command) {
        return true;
    }
    pipeline_command_names(command)
        .iter()
        .any(|command| is_interesting_file_command(command))
}

fn unknown_candidate(command: &str, reason: &str) -> ParseDecision {
    let pipeline_commands = pipeline_command_names(command);
    let first_command = pipeline_commands.first().cloned().or_else(|| {
        tokenize(command)
            .and_then(|tokens| tokens.first().cloned())
            .map(|command| normalize_command_name(&command))
    });
    ParseDecision::UnknownCandidate(UnknownCommand {
        reason: reason.to_string(),
        first_command,
        normalized_shape: normalized_shape(command),
        pipeline_commands,
    })
}

fn pipeline_command_names(command: &str) -> Vec<String> {
    split_pipeline(command)
        .unwrap_or_else(|| vec![command])
        .into_iter()
        .filter_map(|part| tokenize(part).and_then(|tokens| tokens.first().cloned()))
        .map(|command| normalize_command_name(&command))
        .collect()
}

fn normalized_shape(command: &str) -> String {
    split_pipeline(command)
        .unwrap_or_else(|| vec![command])
        .into_iter()
        .filter_map(|part| tokenize(part))
        .filter(|tokens| !tokens.is_empty())
        .map(|tokens| {
            tokens
                .into_iter()
                .map(|token| {
                    let normalized = normalize_command_name(&token);
                    if normalized.starts_with('-') {
                        normalized
                    } else if is_interesting_file_command(&normalized)
                        || is_external_program_command(&normalized)
                    {
                        normalized
                    } else if token.parse::<usize>().is_ok() {
                        "<number>".to_string()
                    } else {
                        "<arg>".to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn normalize_command_name(command: &str) -> String {
    command
        .trim_matches('&')
        .trim_matches('"')
        .trim_matches('\'')
        .to_lowercase()
}

fn parse_get_child_item_find(command: &str, cwd: Option<&str>) -> Option<FastOperation> {
    if command.contains('|') {
        return None;
    }

    let tokens = tokenize(command)?;
    let command_name = tokens.first()?.to_lowercase();
    if !matches!(
        command_name.as_str(),
        "get-childitem" | "gci" | "dir" | "ls"
    ) {
        return None;
    }

    let parsed = parse_gci_options(&tokens[1..], cwd)?;
    if !parsed.recurse {
        return None;
    }
    let pattern = parsed.filter.or(parsed.include)?;

    Some(FastOperation::Find(FindQuery {
        root: parsed.root,
        pattern,
        files_only: true,
        limit: 80,
    }))
}

fn parse_command_list(command: &str, cwd: Option<&str>) -> Option<FastOperation> {
    let parts = split_command_list(command)?;
    if parts.len() <= 1 {
        return None;
    }
    let mut segments = Vec::new();
    for part in parts {
        if let Some(external) = parse_readonly_external(part) {
            segments.push(FastSegment::ReadOnlyExternal(external));
            continue;
        }
        match classify_single_command(part, cwd) {
            ParseDecision::Fast(FastOperation::CommandList(_)) => return None,
            ParseDecision::Fast(operation) => {
                segments.push(FastSegment::Operation(Box::new(operation)))
            }
            ParseDecision::PassThrough(_) | ParseDecision::UnknownCandidate(_) => return None,
        }
    }
    Some(FastOperation::CommandList(segments))
}

fn classify_single_command(command: &str, cwd: Option<&str>) -> ParseDecision {
    let trimmed = command.trim();
    if has_dynamic_or_unsafe_constructs(trimmed) {
        return ParseDecision::PassThrough(PassThroughReason::DynamicOrUnsafe);
    }
    if !is_valid_powershell_ast(trimmed) {
        return unknown_candidate(trimmed, "invalid_or_unsupported_powershell_shape");
    }
    if let Some(operation) = parse_get_content_slice(trimmed) {
        return ParseDecision::Fast(operation);
    }
    if let Some(operation) = parse_get_content_select_string(trimmed) {
        return ParseDecision::Fast(operation);
    }
    if let Some(operation) = parse_find_projection(trimmed, cwd) {
        return ParseDecision::Fast(operation);
    }
    if let Some(operation) = parse_get_child_item_list(trimmed, cwd) {
        return ParseDecision::Fast(operation);
    }
    if let Some(operation) = parse_select_string(trimmed, cwd) {
        return ParseDecision::Fast(operation);
    }
    if let Some(operation) = parse_get_content_inspect(trimmed) {
        return ParseDecision::Fast(operation);
    }
    if let Some(operation) = parse_get_child_item_find(trimmed, cwd) {
        return ParseDecision::Fast(operation);
    }
    unknown_candidate(trimmed, "unsupported_command_list_segment")
}

fn parse_select_string(command: &str, cwd: Option<&str>) -> Option<FastOperation> {
    if command.contains('|') {
        return parse_gci_select_string_pipeline(command, cwd);
    }

    let tokens = tokenize(command)?;
    let command_name = tokens.first()?.to_lowercase();
    if !matches!(command_name.as_str(), "select-string" | "sls") {
        return None;
    }

    let mut pattern = None;
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut case_sensitive = false;
    let mut simple_match = false;
    let mut context = None;
    let mut positional = Vec::new();
    let mut index = 1;
    while index < tokens.len() {
        match tokens[index].to_lowercase().as_str() {
            "-pattern" => {
                index += 1;
                pattern = tokens.get(index).cloned();
            }
            "-path" | "-literalpath" => {
                index += 1;
                if let Some(value) = tokens.get(index) {
                    roots.push(PathBuf::from(value));
                }
            }
            "-casesensitive" => case_sensitive = true,
            "-simplematch" => simple_match = true,
            "-context" => {
                index += 1;
                context = Some(parse_context(tokens.get(index)?)?);
            }
            value if value.starts_with('-') => return None,
            _ => positional.push(tokens[index].clone()),
        }
        index += 1;
    }

    if pattern.is_none() && !positional.is_empty() {
        pattern = Some(positional.remove(0));
    }
    if roots.is_empty() && !positional.is_empty() {
        roots.extend(positional.into_iter().map(PathBuf::from));
    }
    if roots.is_empty() {
        roots.push(PathBuf::from(cwd?));
    }

    let pattern = pattern?;
    if let Some((before, after)) = context {
        Some(FastOperation::GrepContext(GrepContextQuery {
            roots,
            pattern,
            before,
            after,
            case_sensitive,
            simple_match,
            limit: 200,
        }))
    } else {
        Some(FastOperation::Grep(GrepQuery {
            roots,
            pattern,
            case_sensitive,
            simple_match,
            limit: 200,
        }))
    }
}

fn parse_get_content_inspect(command: &str) -> Option<FastOperation> {
    if command.contains('|') {
        return None;
    }
    let tokens = tokenize(command)?;
    let command_name = tokens.first()?.to_lowercase();
    if !matches!(command_name.as_str(), "get-content" | "gc" | "cat" | "type") {
        return None;
    }
    let parsed = parse_get_content_inspect_options(&tokens[1..])?;
    Some(FastOperation::InspectFile {
        path: PathBuf::from(parsed.path),
        raw: parsed.raw,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InspectOptions {
    path: String,
    raw: bool,
}

fn parse_get_content_inspect_options(tokens: &[String]) -> Option<InspectOptions> {
    let mut raw = false;
    let mut path = None;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].to_lowercase().as_str() {
            "-raw" => raw = true,
            "-path" | "-literalpath" => {
                index += 1;
                path = tokens.get(index).cloned();
            }
            "-readcount" | "-totalcount" | "-tail" | "-wait" => return None,
            value if value.starts_with("-path:") || value.starts_with("-literalpath:") => {
                path = tokens[index]
                    .split_once(':')
                    .map(|(_, value)| value.to_string());
            }
            value if value.starts_with('-') => return None,
            _ => positional.push(tokens[index].clone()),
        }
        index += 1;
    }
    if path.is_none() && positional.len() == 1 {
        path = positional.pop();
    }
    if !positional.is_empty() {
        return None;
    }
    Some(InspectOptions { path: path?, raw })
}

fn parse_get_content_slice(command: &str) -> Option<FastOperation> {
    let parts = split_pipeline(command)?;
    if parts.len() != 2 {
        return None;
    }

    let left = tokenize(parts[0])?;
    let right = tokenize(parts[1])?;
    let left_name = left.first()?.to_lowercase();
    let right_name = right.first()?.to_lowercase();
    if !matches!(left_name.as_str(), "get-content" | "gc" | "cat" | "type") {
        return None;
    }
    if !matches!(right_name.as_str(), "select-object" | "select") {
        return None;
    }

    let path = parse_get_content_path(&left[1..])?;
    let (skip, first) = parse_select_object_window(&right[1..])?;
    Some(FastOperation::Slice {
        path: PathBuf::from(path),
        skip,
        first,
    })
}

fn parse_get_content_select_string(command: &str) -> Option<FastOperation> {
    let parts = split_pipeline(command)?;
    if parts.len() != 2 {
        return None;
    }
    let left = tokenize(parts[0])?;
    let right = tokenize(parts[1])?;
    let left_name = left.first()?.to_lowercase();
    let right_name = right.first()?.to_lowercase();
    if !matches!(left_name.as_str(), "get-content" | "gc" | "cat" | "type") {
        return None;
    }
    if !matches!(right_name.as_str(), "select-string" | "sls") {
        return None;
    }
    if !right
        .iter()
        .any(|token| token.eq_ignore_ascii_case("-context"))
    {
        return None;
    }
    let path = parse_get_content_path(&left[1..])?;
    let select = parse_select_string_options(&right[1..])?;
    Some(FastOperation::GrepContext(GrepContextQuery {
        roots: vec![PathBuf::from(path)],
        pattern: select.pattern,
        before: select.before,
        after: select.after,
        case_sensitive: select.case_sensitive,
        simple_match: select.simple_match,
        limit: 200,
    }))
}

fn parse_gci_select_string_pipeline(command: &str, cwd: Option<&str>) -> Option<FastOperation> {
    let parts = split_pipeline(command)?;
    if parts.len() != 2 {
        return None;
    }
    let left = tokenize(parts[0])?;
    let right = tokenize(parts[1])?;
    let left_name = left.first()?.to_lowercase();
    let right_name = right.first()?.to_lowercase();
    if !matches!(left_name.as_str(), "get-childitem" | "gci" | "dir" | "ls") {
        return None;
    }
    if !matches!(right_name.as_str(), "select-string" | "sls") {
        return None;
    }

    let parsed = parse_gci_options(&left[1..], cwd)?;
    if !parsed.recurse {
        return None;
    }
    let root = parsed.root.or_else(|| cwd.map(ToOwned::to_owned))?;
    let pattern = parse_select_string_pattern(&right[1..])?;
    Some(FastOperation::Grep(GrepQuery {
        roots: vec![PathBuf::from(root)],
        pattern,
        case_sensitive: right
            .iter()
            .any(|token| token.eq_ignore_ascii_case("-casesensitive")),
        simple_match: right
            .iter()
            .any(|token| token.eq_ignore_ascii_case("-simplematch")),
        limit: 200,
    }))
}

fn parse_find_projection(command: &str, cwd: Option<&str>) -> Option<FastOperation> {
    let parts = split_pipeline(command)?;
    if !(parts.len() == 2 || parts.len() == 3) {
        return None;
    }
    if parts.len() == 3 {
        let format_tokens = tokenize(parts[2])?;
        let format_name = format_tokens.first()?.to_lowercase();
        if !matches!(format_name.as_str(), "format-table" | "ft") {
            return None;
        }
    }
    let left = tokenize(parts[0])?;
    let select = tokenize(parts[1])?;
    let left_name = left.first()?.to_lowercase();
    let select_name = select.first()?.to_lowercase();
    if !matches!(left_name.as_str(), "get-childitem" | "gci" | "dir" | "ls") {
        return None;
    }
    if !matches!(select_name.as_str(), "select-object" | "select") {
        return None;
    }
    let parsed = parse_gci_options(&left[1..], cwd)?;
    let projection = parse_select_projection(&select[1..])?;
    Some(FastOperation::FindProjection(FindProjection {
        query: FindQuery {
            root: parsed.root,
            pattern: parsed
                .filter
                .or(parsed.include)
                .unwrap_or_else(|| "*".to_string()),
            files_only: true,
            limit: 200,
        },
        projection,
    }))
}

fn parse_get_child_item_list(command: &str, cwd: Option<&str>) -> Option<FastOperation> {
    if command.contains('|') {
        return None;
    }
    let tokens = tokenize(command)?;
    let command_name = tokens.first()?.to_lowercase();
    if !matches!(
        command_name.as_str(),
        "get-childitem" | "gci" | "dir" | "ls"
    ) {
        return None;
    }
    let parsed = parse_gci_options(&tokens[1..], cwd)?;
    let pattern = parsed.filter.or(parsed.include)?;
    Some(FastOperation::FindProjection(FindProjection {
        query: FindQuery {
            root: parsed.root,
            pattern,
            files_only: true,
            limit: 200,
        },
        projection: Projection::FullNameOnly,
    }))
}

#[derive(Debug, Default)]
struct GciOptions {
    root: Option<String>,
    filter: Option<String>,
    include: Option<String>,
    recurse: bool,
}

fn parse_gci_options(tokens: &[String], cwd: Option<&str>) -> Option<GciOptions> {
    let mut options = GciOptions::default();
    let mut positional = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].to_lowercase().as_str() {
            "-recurse" | "-r" => options.recurse = true,
            "-file" | "-force" | "/a-d" => {}
            "-path" | "-literalpath" => {
                index += 1;
                options.root = tokens.get(index).cloned();
            }
            "-filter" => {
                index += 1;
                options.filter = tokens.get(index).cloned();
            }
            "-include" => {
                index += 1;
                options.include = tokens.get(index).cloned();
            }
            "-directory" | "/ad" => return None,
            value if value.starts_with("-path:") || value.starts_with("-literalpath:") => {
                options.root = tokens[index]
                    .split_once(':')
                    .map(|(_, value)| value.to_string());
            }
            value if value.starts_with("-filter:") => {
                options.filter = tokens[index]
                    .split_once(':')
                    .map(|(_, value)| value.to_string());
            }
            value if value.starts_with("-include:") => {
                options.include = tokens[index]
                    .split_once(':')
                    .map(|(_, value)| value.to_string());
            }
            value if value.starts_with('-') => return None,
            _ => positional.push(tokens[index].clone()),
        }
        index += 1;
    }

    if options.root.is_none() && !positional.is_empty() {
        options.root = Some(positional.remove(0));
    }
    if options.root.is_none() {
        options.root = cwd.map(ToOwned::to_owned);
    }
    if !positional.is_empty() {
        return None;
    }
    Some(options)
}

fn parse_select_string_pattern(tokens: &[String]) -> Option<String> {
    let mut index = 0;
    let mut positional = Vec::new();
    while index < tokens.len() {
        match tokens[index].to_lowercase().as_str() {
            "-pattern" => {
                index += 1;
                return tokens.get(index).cloned();
            }
            "-casesensitive" | "-simplematch" => {}
            value if value.starts_with('-') => return None,
            _ => positional.push(tokens[index].clone()),
        }
        index += 1;
    }
    positional.into_iter().next()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectStringOptions {
    pattern: String,
    before: usize,
    after: usize,
    case_sensitive: bool,
    simple_match: bool,
}

fn parse_select_string_options(tokens: &[String]) -> Option<SelectStringOptions> {
    let mut pattern = None;
    let mut before = 0;
    let mut after = 0;
    let mut case_sensitive = false;
    let mut simple_match = false;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].to_lowercase().as_str() {
            "-pattern" => {
                index += 1;
                pattern = tokens.get(index).cloned();
            }
            "-context" => {
                index += 1;
                let parsed = parse_context(tokens.get(index)?)?;
                before = parsed.0;
                after = parsed.1;
            }
            "-casesensitive" => case_sensitive = true,
            "-simplematch" => simple_match = true,
            value if value.starts_with('-') => return None,
            _ => positional.push(tokens[index].clone()),
        }
        index += 1;
    }
    if pattern.is_none() && !positional.is_empty() {
        pattern = Some(positional.remove(0));
    }
    Some(SelectStringOptions {
        pattern: pattern?,
        before,
        after,
        case_sensitive,
        simple_match,
    })
}

fn parse_context(value: &str) -> Option<(usize, usize)> {
    if let Some((before, after)) = value.split_once(',') {
        return Some((before.parse().ok()?, after.parse().ok()?));
    }
    let count = value.parse().ok()?;
    Some((count, count))
}

fn parse_select_projection(tokens: &[String]) -> Option<Projection> {
    if tokens.len() >= 2 && tokens[0].eq_ignore_ascii_case("-expandproperty") {
        if tokens[1].eq_ignore_ascii_case("fullname") {
            return Some(Projection::FullNameOnly);
        }
        return None;
    }
    let joined = tokens.join(" ");
    let mut fields = Vec::new();
    for raw in joined
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let field = match raw.to_lowercase().as_str() {
            "fullname" => MetadataField::FullName,
            "name" => MetadataField::Name,
            "length" => MetadataField::Length,
            "lastwritetime" => MetadataField::LastWriteTime,
            "mode" => MetadataField::Mode,
            _ => return None,
        };
        fields.push(field);
    }
    if fields.is_empty() {
        None
    } else {
        Some(Projection::Metadata { fields })
    }
}

fn parse_readonly_external(command: &str) -> Option<ReadOnlyExternal> {
    let tokens = tokenize(command)?;
    let program = tokens.first()?.to_lowercase();
    if program != "git" {
        return None;
    }
    let args = tokens[1..].to_vec();
    let external = ReadOnlyExternal { program, args };
    if is_readonly_external_shape(&external) {
        Some(external)
    } else {
        None
    }
}

fn is_readonly_external_shape(external: &ReadOnlyExternal) -> bool {
    if external.program != "git" {
        return false;
    }
    match external.args.as_slice() {
        [status, flag] if status == "status" && flag == "--short" => true,
        [status, flag] if status == "status" && flag == "--porcelain" => true,
        [status, flag] if status == "status" && flag == "--porcelain=v1" => true,
        [status, flag] if status == "status" && flag == "--porcelain=v2" => true,
        [diff, flag] if diff == "diff" && flag == "--stat" => true,
        [log, oneline, nflag, value] if log == "log" && oneline == "--oneline" && nflag == "-n" => {
            value
                .parse::<usize>()
                .is_ok_and(|value| (1..=50).contains(&value))
        }
        _ => false,
    }
}

fn parse_get_content_path(tokens: &[String]) -> Option<String> {
    let mut positional = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].to_lowercase().as_str() {
            "-path" | "-literalpath" => {
                index += 1;
                return tokens.get(index).cloned();
            }
            "-raw" | "-readcount" | "-totalcount" | "-tail" | "-wait" => return None,
            value if value.starts_with("-path:") || value.starts_with("-literalpath:") => {
                return tokens[index]
                    .split_once(':')
                    .map(|(_, value)| value.to_string());
            }
            value if value.starts_with('-') => return None,
            _ => positional.push(tokens[index].clone()),
        }
        index += 1;
    }
    if positional.len() == 1 {
        positional.into_iter().next()
    } else {
        None
    }
}

fn parse_select_object_window(tokens: &[String]) -> Option<(usize, usize)> {
    let mut skip = None;
    let mut first = None;
    let mut index = 0;
    while index < tokens.len() {
        match tokens[index].to_lowercase().as_str() {
            "-skip" => {
                index += 1;
                skip = Some(tokens.get(index)?.parse().ok()?);
            }
            "-first" => {
                index += 1;
                first = Some(tokens.get(index)?.parse().ok()?);
            }
            value if value.starts_with('-') => return None,
            _ => return None,
        }
        index += 1;
    }
    Some((skip.unwrap_or(0), first?))
}

fn split_pipeline(command: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote = None;
    for (index, character) in command.char_indices() {
        match character {
            '\'' | '"' if quote == Some(character) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(character),
            '|' if quote.is_none() => {
                parts.push(command[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(command[start..].trim());
    if parts.iter().all(|part| !part.is_empty()) {
        Some(parts)
    } else {
        None
    }
}

fn split_command_list(command: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote = None;
    for (index, character) in command.char_indices() {
        match character {
            '\'' | '"' if quote == Some(character) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(character),
            ';' if quote.is_none() => {
                parts.push(command[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(command[start..].trim());
    if parts.iter().all(|part| !part.is_empty()) {
        Some(parts)
    } else {
        None
    }
}

fn tokenize(command: &str) -> Option<Vec<String>> {
    let pattern = Regex::new(r#""([^"]*)"|'([^']*)'|(\S+)"#).ok()?;
    let tokens: Vec<String> = pattern
        .captures_iter(command)
        .filter_map(|captures| {
            captures
                .get(1)
                .or_else(|| captures.get(2))
                .or_else(|| captures.get(3))
                .map(|matched| matched.as_str().to_string())
        })
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recursive_filter_find() {
        let operation = parse_powershell(
            "Get-ChildItem C:\\repo -Recurse -Filter *.rs",
            Some("C:\\fallback"),
        )
        .unwrap();

        assert_eq!(
            operation,
            FastOperation::Find(FindQuery {
                root: Some("C:\\repo".to_string()),
                pattern: "*.rs".to_string(),
                files_only: true,
                limit: 80,
            })
        );
    }

    #[test]
    fn parses_select_string_path_pattern() {
        let operation = parse_powershell(
            "Select-String -Path C:\\repo\\*.txt -Pattern needle",
            Some("C:\\repo"),
        )
        .unwrap();

        match operation {
            FastOperation::Grep(query) => {
                assert_eq!(query.pattern, "needle");
                assert_eq!(query.roots, vec![PathBuf::from("C:\\repo\\*.txt")]);
            }
            other => panic!("unexpected operation: {other:?}"),
        }
    }

    #[test]
    fn parses_get_child_item_pipe_select_string() {
        let operation = parse_powershell(
            "Get-ChildItem C:\\repo -Recurse -Filter *.txt | Select-String -Pattern needle",
            Some("C:\\fallback"),
        )
        .unwrap();

        match operation {
            FastOperation::Grep(query) => {
                assert_eq!(query.pattern, "needle");
                assert_eq!(query.roots, vec![PathBuf::from("C:\\repo")]);
            }
            other => panic!("unexpected operation: {other:?}"),
        }
    }

    #[test]
    fn parses_get_content_slice() {
        let operation = parse_powershell(
            "Get-Content C:\\repo\\file.txt | Select-Object -Skip 2 -First 3",
            None,
        )
        .unwrap();

        assert_eq!(
            operation,
            FastOperation::Slice {
                path: PathBuf::from("C:\\repo\\file.txt"),
                skip: 2,
                first: 3,
            }
        );
    }

    #[test]
    fn fail_opens_for_mutating_commands() {
        assert!(parse_powershell("Remove-Item C:\\repo -Recurse", Some("C:\\repo")).is_none());
    }

    #[test]
    fn classifies_supported_commands_as_fast() {
        assert!(matches!(
            classify_powershell("gci C:\\repo -Recurse -Filter *.rs", Some("C:\\repo")),
            ParseDecision::Fast(FastOperation::Find(_))
        ));
        assert!(matches!(
            classify_powershell(
                "Select-String -Path C:\\repo\\*.rs -Pattern needle",
                Some("C:\\repo")
            ),
            ParseDecision::Fast(FastOperation::Grep(_))
        ));
        assert!(matches!(
            classify_powershell(
                "Get-Content C:\\repo\\file.txt | Select-Object -Skip 2 -First 3",
                Some("C:\\repo")
            ),
            ParseDecision::Fast(FastOperation::Slice { .. })
        ));
    }

    #[test]
    fn classifies_mutating_and_dynamic_commands_as_pass_through() {
        assert!(matches!(
            classify_powershell("Remove-Item C:\\repo -Recurse", Some("C:\\repo")),
            ParseDecision::PassThrough(PassThroughReason::Mutating)
        ));
        assert!(matches!(
            classify_powershell(
                "Get-ChildItem $(Get-Location) -Recurse -Filter *.rs",
                Some("C:\\repo")
            ),
            ParseDecision::PassThrough(PassThroughReason::DynamicOrUnsafe)
        ));
    }

    #[test]
    fn classifies_external_programs_as_pass_through() {
        for command in [
            "git status",
            "rg needle .",
            "cargo test",
            "python script.py",
        ] {
            assert!(
                matches!(
                    classify_powershell(command, Some("C:\\repo")),
                    ParseDecision::PassThrough(PassThroughReason::ExternalProgram)
                ),
                "{command}"
            );
        }
    }

    #[test]
    fn classifies_complex_file_pipelines_as_unknown_candidates() {
        let decision = classify_powershell(
            "gci C:\\repo -Recurse -Filter *.rs | Where-Object Name -like '*main*' | Select-String needle",
            Some("C:\\repo"),
        );

        let ParseDecision::UnknownCandidate(detail) = decision else {
            panic!("unexpected decision: {decision:?}");
        };
        assert_eq!(detail.first_command.as_deref(), Some("gci"));
        assert_eq!(
            detail.pipeline_commands,
            vec!["gci", "where-object", "select-string"]
        );
        assert!(detail.normalized_shape.contains("gci"));
    }

    #[test]
    fn classifies_unsupported_file_commands_as_unknown_candidates() {
        for command in [
            "Get-Content C:\\repo\\file.txt | Select-String needle",
            "gci C:\\repo -Recurse | Sort-Object Name",
        ] {
            assert!(
                matches!(
                    classify_powershell(command, Some("C:\\repo")),
                    ParseDecision::UnknownCandidate(_)
                ),
                "{command}"
            );
        }
    }

    #[test]
    fn parses_get_content_full_read_forms_as_inspect_file() {
        for command in [
            "Get-Content file.py",
            "Get-Content -Raw file.toml",
            "Get-Content -Path src/lib.rs",
            "Get-Content -LiteralPath 'x.md'",
            "gc x.txt",
        ] {
            assert!(
                matches!(
                    classify_powershell(command, Some("C:\\repo")),
                    ParseDecision::Fast(FastOperation::InspectFile { .. })
                ),
                "{command}"
            );
        }
    }

    #[test]
    fn parses_select_string_context_forms() {
        assert!(matches!(
            classify_powershell(
                "Get-Content file.txt | Select-String -Pattern needle -Context 2,3",
                Some("C:\\repo")
            ),
            ParseDecision::Fast(FastOperation::GrepContext(_))
        ));
        assert!(matches!(
            classify_powershell(
                "Select-String -Path file.txt -Pattern needle -Context 0,1",
                Some("C:\\repo")
            ),
            ParseDecision::Fast(FastOperation::GrepContext(_))
        ));
    }

    #[test]
    fn parses_find_projection_forms() {
        assert!(matches!(
            classify_powershell(
                "Get-ChildItem -Recurse -Filter AGENTS.md | Select-Object -ExpandProperty FullName",
                Some("C:\\repo")
            ),
            ParseDecision::Fast(FastOperation::FindProjection(_))
        ));
        assert!(matches!(
            classify_powershell(
                "Get-ChildItem -Recurse -Filter *.rs | Select-Object FullName,Length,LastWriteTime | Format-Table -AutoSize",
                Some("C:\\repo")
            ),
            ParseDecision::Fast(FastOperation::FindProjection(_))
        ));
    }

    #[test]
    fn parses_safe_command_list_with_readonly_git() {
        let decision = classify_powershell(
            "Get-ChildItem -Force -Filter AGENTS.md; git status --short",
            Some("C:\\repo"),
        );

        let ParseDecision::Fast(FastOperation::CommandList(segments)) = decision else {
            panic!("unexpected decision: {decision:?}");
        };
        assert_eq!(segments.len(), 2);
    }

    #[test]
    fn command_list_with_unknown_external_fails_open() {
        assert!(matches!(
            classify_powershell(
                "Get-ChildItem -Force -Filter AGENTS.md; npm install",
                Some("C:\\repo")
            ),
            ParseDecision::UnknownCandidate(_)
        ));
    }
}
