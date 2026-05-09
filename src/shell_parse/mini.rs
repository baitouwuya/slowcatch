use super::{
    FastOperation, FastSegment, ParseDecision, classify_single_command,
    has_dynamic_or_unsafe_constructs, is_mutating_command, normalize_command_name,
    parse_readonly_external, tokenize,
};
use std::collections::HashMap;
use std::path::PathBuf;
use tree_sitter::Parser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiniScript {
    pub branches: Vec<MiniBranch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiniBranch {
    pub condition: Option<MiniCondition>,
    pub segments: Vec<FastSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MiniCondition {
    TestPath {
        path: PathBuf,
        path_type: TestPathType,
    },
    Not(Box<MiniCondition>),
    And(Box<MiniCondition>, Box<MiniCondition>),
    Or(Box<MiniCondition>, Box<MiniCondition>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestPathType {
    Any,
    Leaf,
    Container,
}

pub fn parse_mini_script(command: &str, cwd: Option<&str>) -> Option<FastOperation> {
    if !looks_like_mini_script(command) {
        return None;
    }
    if has_dynamic_or_unsafe_constructs(command) {
        return None;
    }
    if !is_valid_powershell_ast(command) {
        return None;
    }
    let mut parser = MiniParser::new(command, cwd);
    let script = parser.parse()?;
    if script.branches.is_empty() {
        None
    } else {
        Some(FastOperation::MiniScript(script))
    }
}

fn looks_like_mini_script(command: &str) -> bool {
    let lowered = command.to_ascii_lowercase();
    lowered.contains("if") && lowered.contains("test-path") && lowered.contains('{')
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

struct MiniParser<'a> {
    command: &'a str,
    cwd: Option<&'a str>,
    variables: HashMap<String, String>,
}

impl<'a> MiniParser<'a> {
    fn new(command: &'a str, cwd: Option<&'a str>) -> Self {
        Self {
            command,
            cwd,
            variables: HashMap::new(),
        }
    }

    fn parse(&mut self) -> Option<MiniScript> {
        let parts = split_top_level_command_list(self.command)?;
        let if_index = parts
            .iter()
            .position(|part| starts_with_keyword(part, "if"))?;
        for assignment in &parts[..if_index] {
            self.parse_literal_assignment(assignment)?;
        }
        if parts[if_index + 1..]
            .iter()
            .any(|part| !part.trim().is_empty())
        {
            return None;
        }
        self.parse_if_chain(parts[if_index])
    }

    fn parse_literal_assignment(&mut self, command: &str) -> Option<()> {
        let (name, value) = command.split_once('=')?;
        let name = name.trim();
        let value = value.trim();
        if !is_simple_variable(name) {
            return None;
        }
        let value = parse_quoted_literal(value)?;
        if contains_unsupported_literal(&value) {
            return None;
        }
        self.variables
            .insert(name.trim_start_matches('$').to_ascii_lowercase(), value);
        Some(())
    }

    fn parse_if_chain(&self, command: &str) -> Option<MiniScript> {
        let mut scanner = Scanner::new(command);
        let mut branches = Vec::new();
        scanner.expect_keyword("if")?;
        let condition = scanner.read_parenthesized()?;
        let body = scanner.read_braced()?;
        branches.push(MiniBranch {
            condition: Some(self.parse_condition(&condition)?),
            segments: self.parse_body(&body)?,
        });
        loop {
            scanner.skip_ws();
            if scanner.is_done() {
                break;
            }
            if scanner.consume_keyword("elseif") {
                let condition = scanner.read_parenthesized()?;
                let body = scanner.read_braced()?;
                branches.push(MiniBranch {
                    condition: Some(self.parse_condition(&condition)?),
                    segments: self.parse_body(&body)?,
                });
            } else if scanner.consume_keyword("else") {
                let body = scanner.read_braced()?;
                branches.push(MiniBranch {
                    condition: None,
                    segments: self.parse_body(&body)?,
                });
                scanner.skip_ws();
                if !scanner.is_done() {
                    return None;
                }
                break;
            } else {
                return None;
            }
        }
        Some(MiniScript { branches })
    }

    fn parse_body(&self, body: &str) -> Option<Vec<FastSegment>> {
        let parts = split_top_level_command_list(body)?;
        let mut segments = Vec::new();
        for part in parts {
            if starts_with_keyword(part, "if") {
                return None;
            }
            let replaced = self.substitute_variables(part)?;
            let first = tokenize(&replaced)
                .and_then(|tokens| tokens.first().cloned())
                .map(|token| normalize_command_name(&token));
            if is_mutating_command(&replaced, first.as_deref()) {
                return None;
            }
            if let Some(external) = parse_readonly_external(&replaced) {
                segments.push(FastSegment::ReadOnlyExternal(external));
                continue;
            }
            match classify_single_command(&replaced, self.cwd) {
                ParseDecision::Fast(FastOperation::CommandList(_))
                | ParseDecision::Fast(FastOperation::MiniScript(_)) => return None,
                ParseDecision::Fast(operation) => {
                    segments.push(FastSegment::Operation(Box::new(operation)))
                }
                ParseDecision::PassThrough(_) | ParseDecision::UnknownCandidate(_) => return None,
            }
        }
        if segments.is_empty() {
            None
        } else {
            Some(segments)
        }
    }

    fn parse_condition(&self, condition: &str) -> Option<MiniCondition> {
        let trimmed = strip_wrapping_parens(condition.trim());
        self.parse_or(trimmed)
    }

    fn parse_or(&self, condition: &str) -> Option<MiniCondition> {
        if let Some((left, right)) = split_binary_condition(condition, "-or") {
            return Some(MiniCondition::Or(
                Box::new(self.parse_or(left)?),
                Box::new(self.parse_and(right)?),
            ));
        }
        self.parse_and(condition)
    }

    fn parse_and(&self, condition: &str) -> Option<MiniCondition> {
        if let Some((left, right)) = split_binary_condition(condition, "-and") {
            return Some(MiniCondition::And(
                Box::new(self.parse_and(left)?),
                Box::new(self.parse_not(right)?),
            ));
        }
        self.parse_not(condition)
    }

    fn parse_not(&self, condition: &str) -> Option<MiniCondition> {
        let trimmed = strip_wrapping_parens(condition.trim());
        if let Some(rest) = trimmed.strip_prefix('!') {
            return Some(MiniCondition::Not(Box::new(self.parse_not(rest)?)));
        }
        if starts_with_keyword(trimmed, "-not") {
            return Some(MiniCondition::Not(Box::new(
                self.parse_not(trimmed[4..].trim())?,
            )));
        }
        self.parse_test_path(trimmed)
    }

    fn parse_test_path(&self, condition: &str) -> Option<MiniCondition> {
        let tokens = tokenize(condition)?;
        let command = tokens.first()?.to_ascii_lowercase();
        if command != "test-path" {
            return None;
        }
        let mut path = None;
        let mut path_type = TestPathType::Any;
        let mut positional = Vec::new();
        let mut index = 1;
        while index < tokens.len() {
            match tokens[index].to_ascii_lowercase().as_str() {
                "-path" | "-literalpath" => {
                    index += 1;
                    path = tokens
                        .get(index)
                        .and_then(|value| self.resolve_value(value));
                }
                "-pathtype" => {
                    index += 1;
                    path_type = parse_path_type(tokens.get(index)?)?;
                }
                value if value.starts_with("-path:") || value.starts_with("-literalpath:") => {
                    path = tokens[index]
                        .split_once(':')
                        .and_then(|(_, value)| self.resolve_value(value));
                }
                value if value.starts_with('-') => return None,
                _ => positional.push(tokens[index].clone()),
            }
            index += 1;
        }
        if path.is_none() && positional.len() == 1 {
            path = self.resolve_value(positional.first()?);
        } else if !positional.is_empty() {
            return None;
        }
        let path = path?;
        if contains_wildcard(&path) {
            return None;
        }
        Some(MiniCondition::TestPath {
            path: PathBuf::from(path),
            path_type,
        })
    }

    fn substitute_variables(&self, command: &str) -> Option<String> {
        let mut output = Vec::new();
        for token in tokenize(command)? {
            if is_simple_variable(&token) {
                output.push(quote_argument(&self.resolve_value(&token)?)?);
            } else if token_contains_variable_or_interpolation(&token) {
                return None;
            } else {
                output.push(token);
            }
        }
        Some(output.join(" "))
    }

    fn resolve_value(&self, value: &str) -> Option<String> {
        if is_simple_variable(value) {
            self.variables
                .get(&value.trim_start_matches('$').to_ascii_lowercase())
                .cloned()
        } else if token_contains_variable_or_interpolation(value) {
            None
        } else {
            let value = value.to_string();
            if contains_unsupported_literal(&value) {
                None
            } else {
                Some(value)
            }
        }
    }
}

fn parse_path_type(value: &str) -> Option<TestPathType> {
    match value.to_ascii_lowercase().as_str() {
        "any" => Some(TestPathType::Any),
        "leaf" => Some(TestPathType::Leaf),
        "container" => Some(TestPathType::Container),
        _ => None,
    }
}

fn parse_quoted_literal(value: &str) -> Option<String> {
    if (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
    {
        Some(value[1..value.len() - 1].to_string())
    } else {
        None
    }
}

fn contains_unsupported_literal(value: &str) -> bool {
    value.contains('$') || value.contains('`') || value.contains('\n') || value.contains('\r')
}

fn quote_argument(value: &str) -> Option<String> {
    if !value.contains('\'') {
        Some(format!("'{value}'"))
    } else if !value.contains('"') {
        Some(format!("\"{value}\""))
    } else {
        None
    }
}

fn token_contains_variable_or_interpolation(value: &str) -> bool {
    value.contains('$') && !is_simple_variable(value)
}

fn is_simple_variable(value: &str) -> bool {
    let Some(name) = value.strip_prefix('$') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn contains_wildcard(value: &str) -> bool {
    value.contains('*') || value.contains('?') || value.contains('[') || value.contains(']')
}

fn split_top_level_command_list(command: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut paren_depth = 0usize;
    let mut brace_depth = 0usize;
    for (index, character) in command.char_indices() {
        match character {
            '\'' | '"' if quote == Some(character) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(character),
            '(' if quote.is_none() => paren_depth += 1,
            ')' if quote.is_none() => paren_depth = paren_depth.checked_sub(1)?,
            '{' if quote.is_none() => brace_depth += 1,
            '}' if quote.is_none() => brace_depth = brace_depth.checked_sub(1)?,
            ';' if quote.is_none() && paren_depth == 0 && brace_depth == 0 => {
                parts.push(command[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    if quote.is_some() || paren_depth != 0 || brace_depth != 0 {
        return None;
    }
    parts.push(command[start..].trim());
    if parts.iter().all(|part| !part.is_empty()) {
        Some(parts)
    } else {
        None
    }
}

fn strip_wrapping_parens(value: &str) -> &str {
    let mut current = value.trim();
    loop {
        if !(current.starts_with('(') && current.ends_with(')')) {
            return current;
        }
        let Some(close) = matching_close(current, 0, '(', ')') else {
            return current;
        };
        if close != current.len() - 1 {
            return current;
        }
        current = current[1..current.len() - 1].trim();
    }
}

fn split_binary_condition<'a>(condition: &'a str, operator: &str) -> Option<(&'a str, &'a str)> {
    let condition = strip_wrapping_parens(condition.trim());
    let bytes = condition.as_bytes();
    let mut depth = 0usize;
    let mut quote = None;
    let mut index = 0usize;
    while index < bytes.len() {
        let character = condition[index..].chars().next()?;
        match character {
            '\'' | '"' if quote == Some(character) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(character),
            '(' if quote.is_none() => depth += 1,
            ')' if quote.is_none() => depth = depth.saturating_sub(1),
            _ => {
                if quote.is_none()
                    && depth == 0
                    && condition[index..].starts_with(operator)
                    && boundary_before(condition, index)
                    && boundary_after(condition, index + operator.len())
                {
                    return Some((
                        condition[..index].trim(),
                        condition[index + operator.len()..].trim(),
                    ));
                }
            }
        }
        index += character.len_utf8();
    }
    None
}

fn boundary_before(value: &str, index: usize) -> bool {
    index == 0
        || value[..index]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
}

fn boundary_after(value: &str, index: usize) -> bool {
    index == value.len()
        || value[index..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
}

fn starts_with_keyword(value: &str, keyword: &str) -> bool {
    let trimmed = value.trim_start();
    if !trimmed
        .get(..keyword.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(keyword))
    {
        return false;
    }
    trimmed
        .get(keyword.len()..)
        .and_then(|rest| rest.chars().next())
        .is_none_or(|character| character.is_whitespace() || character == '(' || character == '{')
}

fn matching_close(value: &str, open_index: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None;
    for (index, character) in value
        .char_indices()
        .skip_while(|(index, _)| *index < open_index)
    {
        match character {
            '\'' | '"' if quote == Some(character) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(character),
            current if current == open && quote.is_none() => depth += 1,
            current if current == close && quote.is_none() => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

struct Scanner<'a> {
    value: &'a str,
    index: usize,
}

impl<'a> Scanner<'a> {
    fn new(value: &'a str) -> Self {
        Self { value, index: 0 }
    }

    fn skip_ws(&mut self) {
        while self
            .value
            .get(self.index..)
            .and_then(|rest| rest.chars().next())
            .is_some_and(char::is_whitespace)
        {
            self.index += self.value[self.index..].chars().next().unwrap().len_utf8();
        }
    }

    fn is_done(&self) -> bool {
        self.index >= self.value.len()
    }

    fn expect_keyword(&mut self, keyword: &str) -> Option<()> {
        if self.consume_keyword(keyword) {
            Some(())
        } else {
            None
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        self.skip_ws();
        let Some(rest) = self.value.get(self.index..) else {
            return false;
        };
        if !starts_with_keyword(rest, keyword) {
            return false;
        }
        self.index += keyword.len();
        true
    }

    fn read_parenthesized(&mut self) -> Option<String> {
        self.read_enclosed('(', ')')
    }

    fn read_braced(&mut self) -> Option<String> {
        self.read_enclosed('{', '}')
    }

    fn read_enclosed(&mut self, open: char, close: char) -> Option<String> {
        self.skip_ws();
        let rest = self.value.get(self.index..)?;
        if !rest.starts_with(open) {
            return None;
        }
        let open_index = self.index;
        let close_index = matching_close(self.value, open_index, open, close)?;
        let content = self.value[open_index + open.len_utf8()..close_index].to_string();
        self.index = close_index + close.len_utf8();
        Some(content)
    }
}
