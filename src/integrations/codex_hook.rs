use crate::backends::directory;
use crate::backends::everything::{FileFinder, TimedEverythingFinder};
use crate::backends::grep;
use crate::backends::inspect_file;
use crate::backends::projection;
use crate::backends::slice;
use crate::integrations::post_edit_check;
use crate::integrations::post_edit_state;
use crate::integrations::project_check::{
    self, ProcessProjectCommandRunner, ProjectCheckRequest, ProjectCommandRunner,
};
use crate::prompt_enrichment;
use crate::shell_parse::{
    FastOperation, FastSegment, MiniCondition, ParseDecision, ReadWindow, TestPathType,
    UnknownCommand, classify_powershell,
};
use crate::unknown_log::HookLogContext;
use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const FAST_PATH_PREFIX: &str = "FAST_PATH_SUCCESS";
const HOOK_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const EVERYTHING_HOOK_TIMEOUT: Duration = Duration::from_millis(1_500);

#[derive(Debug, Deserialize)]
struct HookInput {
    hook_event_name: Option<String>,
    session_id: Option<String>,
    turn_id: Option<String>,
    model: Option<String>,
    prompt: Option<String>,
    cwd: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<ToolInput>,
    tool_response: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ToolInput {
    command: Option<String>,
}

pub fn run_from_stdin() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let finder = Arc::new(TimedEverythingFinder::new(EVERYTHING_HOOK_TIMEOUT));
    if let Some(output) = handle_hook_json_with_log_path_and_timeout(
        &input,
        finder,
        &crate::unknown_log::default_log_path(),
        HOOK_OPERATION_TIMEOUT,
    )? {
        println!("{output}");
    }
    Ok(())
}

pub fn handle_hook_json(input: &str, finder: &dyn FileFinder) -> Result<Option<String>> {
    handle_hook_json_with_log_path(input, finder, &crate::unknown_log::default_log_path())
}

pub fn handle_hook_json_with_log_path(
    input: &str,
    finder: &dyn FileFinder,
    unknown_log_path: &std::path::Path,
) -> Result<Option<String>> {
    let runner = ProcessProjectCommandRunner;
    handle_hook_json_inner(
        input,
        finder,
        unknown_log_path,
        &post_edit_state::default_state_path(),
        &runner,
        |operation, cwd| execute_operation(operation, finder, cwd),
    )
}

#[cfg(test)]
fn handle_hook_json_with_state_and_runner(
    input: &str,
    finder: &dyn FileFinder,
    state_path: &Path,
    runner: &dyn ProjectCommandRunner,
) -> Result<Option<String>> {
    handle_hook_json_inner(
        input,
        finder,
        &crate::unknown_log::default_log_path(),
        state_path,
        runner,
        |operation, cwd| execute_operation(operation, finder, cwd),
    )
}

pub fn handle_hook_json_with_log_path_and_timeout(
    input: &str,
    finder: Arc<dyn FileFinder>,
    unknown_log_path: &std::path::Path,
    timeout: Duration,
) -> Result<Option<String>> {
    let runner = ProcessProjectCommandRunner;
    handle_hook_json_inner(
        input,
        finder.as_ref(),
        unknown_log_path,
        &post_edit_state::default_state_path(),
        &runner,
        |operation, cwd| {
            execute_operation_with_timeout(operation, Arc::clone(&finder), cwd, timeout)
        },
    )
}

fn handle_hook_json_inner<F>(
    input: &str,
    finder: &dyn FileFinder,
    unknown_log_path: &std::path::Path,
    post_edit_state_path: &Path,
    project_runner: &dyn ProjectCommandRunner,
    execute: F,
) -> Result<Option<String>>
where
    F: Fn(&FastOperation, Option<&str>) -> Result<HookResult>,
{
    let _ = unknown_log_path;
    let hook_input: HookInput = serde_json::from_str(input)?;
    let context = log_context(&hook_input);
    if is_user_prompt_submit(&hook_input) {
        if let Some(prompt) = hook_input.prompt.as_deref() {
            if let Some(additional_context) = prompt_enrichment::enrich_prompt(prompt, finder) {
                return Ok(Some(
                    json!({
                        "hookSpecificOutput": {
                            "hookEventName": "UserPromptSubmit",
                            "additionalContext": additional_context
                        }
                    })
                    .to_string(),
                ));
            }
        }
        return Ok(None);
    }

    if is_stop(&hook_input) {
        return Ok(handle_stop(
            &hook_input,
            post_edit_state_path,
            project_runner,
        ));
    }

    if is_post_tool_use_apply_patch(&hook_input) {
        return Ok(handle_post_tool_use_apply_patch(
            &hook_input,
            post_edit_state_path,
        ));
    }

    if !is_pre_tool_use_or_legacy_shell(&hook_input) {
        return Ok(None);
    }

    let Some(command) = hook_input
        .tool_input
        .as_ref()
        .and_then(|tool| tool.command.clone())
    else {
        return Ok(None);
    };

    match classify_powershell(&command, hook_input.cwd.as_deref()) {
        ParseDecision::Fast(operation) => {
            let output = match execute(&operation, hook_input.cwd.as_deref()) {
                Ok(output) => output,
                Err(error) => {
                    log_fast_path_failure(unknown_log_path, &command, &operation, &context, &error);
                    return Ok(None);
                }
            };
            Ok(Some(
                json!({
                    "decision": "block",
                    "reason": format!(
                        "{FAST_PATH_PREFIX}: executed slowcatch substitute output; do not retry the original command.\n\n{}",
                        output.render()
                    )
                })
                .to_string(),
            ))
        }
        ParseDecision::UnknownCandidate(detail) => {
            let _ = crate::unknown_log::append_unknown_command(
                unknown_log_path,
                &command,
                &detail,
                &context,
            );
            Ok(None)
        }
        ParseDecision::PassThrough(_) => Ok(None),
    }
}

fn is_user_prompt_submit(input: &HookInput) -> bool {
    match input.hook_event_name.as_deref() {
        Some("UserPromptSubmit") => true,
        Some(_) => false,
        None => input.prompt.is_some() && input.tool_input.is_none(),
    }
}

fn is_post_tool_use_apply_patch(input: &HookInput) -> bool {
    input.hook_event_name.as_deref() == Some("PostToolUse")
        && input.tool_name.as_deref() == Some("apply_patch")
}

fn is_stop(input: &HookInput) -> bool {
    input.hook_event_name.as_deref() == Some("Stop")
}

fn is_pre_tool_use_or_legacy_shell(input: &HookInput) -> bool {
    match input.hook_event_name.as_deref() {
        Some("PreToolUse") => input
            .tool_name
            .as_deref()
            .is_none_or(|tool_name| tool_name == "Bash"),
        Some(_) => false,
        None => input.tool_input.is_some(),
    }
}

fn handle_post_tool_use_apply_patch(
    input: &HookInput,
    post_edit_state_path: &Path,
) -> Option<String> {
    if !post_edit_check::apply_patch_succeeded(input.tool_response.as_ref()) {
        return None;
    }
    let command = input.tool_input.as_ref()?.command.as_deref()?;
    if let Some(paths) =
        post_edit_check::touched_paths_from_apply_patch(command, input.cwd.as_deref())
    {
        let _ = post_edit_state::append_touched_paths(
            post_edit_state_path,
            input.session_id.as_deref(),
            input.turn_id.as_deref(),
            input.cwd.as_deref(),
            &paths,
        );
    }
    let report = post_edit_check::check_apply_patch_command(command, input.cwd.as_deref())?;
    let findings = report.render();
    Some(
        json!({
            "decision": "block",
            "reason": format!("slowcatch_post_edit_check v1 issues\n\n{findings}"),
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": findings
            }
        })
        .to_string(),
    )
}

fn handle_stop(
    input: &HookInput,
    post_edit_state_path: &Path,
    project_runner: &dyn ProjectCommandRunner,
) -> Option<String> {
    let cwd = input.cwd.as_deref()?;
    let touched_paths = post_edit_state::read_touched_paths(
        post_edit_state_path,
        input.session_id.as_deref(),
        input.turn_id.as_deref(),
    )
    .ok()?;
    if touched_paths.is_empty() {
        return None;
    }
    let failures = project_check::run_project_checks(
        ProjectCheckRequest {
            cwd: cwd.into(),
            touched_paths,
        },
        project_runner,
    )
    .ok()?;
    if failures.is_empty() {
        return None;
    }
    let rendered = project_check::render_project_check_failures(&failures);
    Some(
        json!({
            "decision": "block",
            "reason": format!("slowcatch_project_check v1 failed\n\n{rendered}")
        })
        .to_string(),
    )
}

fn log_context(input: &HookInput) -> HookLogContext {
    HookLogContext {
        session_id: input.session_id.clone(),
        turn_id: input.turn_id.clone(),
        model: input.model.clone(),
        cwd: input.cwd.clone(),
    }
}

fn execute_operation(
    operation: &FastOperation,
    finder: &dyn FileFinder,
    cwd: Option<&str>,
) -> Result<HookResult> {
    match operation {
        FastOperation::Find(query) => Ok(HookResult::from_lines(
            operation_kind(operation),
            finder.find(query)?,
        )),
        FastOperation::FindProjection(projection) => {
            let paths = finder.find(&projection.query)?;
            Ok(HookResult::from_body(
                operation_kind(operation),
                projection::render_projection(&paths, &projection.projection)?,
            ))
        }
        FastOperation::FindPipeline(pipeline) => {
            let paths = finder.find(&pipeline.query)?;
            let paths = projection::apply_find_pipeline(paths, pipeline);
            Ok(HookResult::from_body(
                operation_kind(operation),
                projection::render_projection(&paths, &pipeline.projection)?,
            ))
        }
        FastOperation::FindMeasure(measure) => {
            let paths = finder.find(&measure.query)?;
            Ok(HookResult::from_body(
                operation_kind(operation),
                projection::render_find_measure(paths, measure),
            ))
        }
        FastOperation::DirectProjection { paths, projection } => Ok(HookResult::from_body(
            operation_kind(operation),
            projection::render_projection(paths, projection)?,
        )),
        FastOperation::Grep(query) => Ok(HookResult::from_lines(
            operation_kind(operation),
            grep::grep(query)?
                .into_iter()
                .map(|item| item.to_tool_text())
                .collect(),
        )),
        FastOperation::GrepContext(query) => Ok(HookResult::from_body(
            operation_kind(operation),
            grep::grep_context(query)?,
        )),
        FastOperation::InspectFile { path, raw } => {
            let inspection = inspect_file::inspect_file_structured(path, *raw)?;
            Ok(HookResult::inspect_file(inspection))
        }
        FastOperation::ReadLines {
            path,
            window,
            count,
        } => {
            let lines = match window {
                ReadWindow::Head => slice::head_lines(path, *count)?,
                ReadWindow::Tail => slice::tail_lines(path, *count)?,
            };
            Ok(HookResult::from_lines(operation_kind(operation), lines))
        }
        FastOperation::Slice { path, skip, first } => Ok(HookResult::from_lines(
            operation_kind(operation),
            slice::slice_lines(path, *skip, *first)?,
        )),
        FastOperation::ListDirectory(options) => Ok(HookResult::from_body(
            operation_kind(operation),
            directory::list_directory(options)?,
        )),
        FastOperation::ListDirectoryPipeline(pipeline) => Ok(HookResult::from_body(
            operation_kind(operation),
            directory::list_directory_projection(pipeline)?,
        )),
        FastOperation::CommandList(segments) => {
            let mut rendered_segments = Vec::new();
            for segment in segments {
                rendered_segments.push(execute_segment(segment, finder, cwd)?);
            }
            Ok(HookResult::segments("command_list", rendered_segments))
        }
        FastOperation::MiniScript(script) => {
            let mut selected = None;
            for branch in &script.branches {
                if branch
                    .condition
                    .as_ref()
                    .map(evaluate_condition)
                    .transpose()?
                    .unwrap_or(true)
                {
                    selected = Some(&branch.segments);
                    break;
                }
            }
            let Some(segments) = selected else {
                return Ok(HookResult::empty("mini_script"));
            };
            let mut rendered_segments = Vec::new();
            for segment in segments {
                rendered_segments.push(execute_segment(segment, finder, cwd)?);
            }
            if rendered_segments.is_empty() {
                Ok(HookResult::empty("mini_script"))
            } else if rendered_segments.len() == 1 {
                let mut result = rendered_segments.remove(0);
                result.kind = "mini_script";
                Ok(result)
            } else {
                Ok(HookResult::segments("mini_script", rendered_segments))
            }
        }
    }
}

fn execute_segment(
    segment: &FastSegment,
    finder: &dyn FileFinder,
    cwd: Option<&str>,
) -> Result<HookResult> {
    match segment {
        FastSegment::Operation(operation) => execute_operation(operation, finder, cwd),
        FastSegment::ReadOnlyExternal(external) => Ok(HookResult::from_body(
            "readonly_external",
            projection::run_read_only_external(external, cwd)?,
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HookResult {
    kind: &'static str,
    status: HookStatus,
    items: Option<usize>,
    omitted: Option<usize>,
    metadata: Vec<String>,
    body: String,
    segments: Vec<HookResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookStatus {
    Ok,
    Empty,
}

impl HookStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Empty => "empty",
        }
    }
}

impl HookResult {
    fn from_lines(kind: &'static str, lines: Vec<String>) -> Self {
        if lines.is_empty() {
            Self::empty(kind)
        } else {
            let items = lines.len();
            Self {
                kind,
                status: HookStatus::Ok,
                items: Some(items),
                omitted: None,
                metadata: Vec::new(),
                body: lines.join("\n"),
                segments: Vec::new(),
            }
        }
    }

    fn from_body(kind: &'static str, body: String) -> Self {
        if body.trim().is_empty() || body.trim() == "no results" {
            return Self::empty(kind);
        }
        let (body, items, omitted) = normalize_body_and_count_items(&body);
        Self {
            kind,
            status: HookStatus::Ok,
            items: Some(items),
            omitted,
            metadata: Vec::new(),
            body,
            segments: Vec::new(),
        }
    }

    fn inspect_file(inspection: inspect_file::FileInspection) -> Self {
        let body_empty = inspection.body.trim().is_empty();
        Self {
            kind: "inspect_file",
            status: if body_empty {
                HookStatus::Empty
            } else {
                HookStatus::Ok
            },
            items: None,
            omitted: None,
            metadata: inspection.header_lines(),
            body: if body_empty {
                "no results".to_string()
            } else {
                inspection.body
            },
            segments: Vec::new(),
        }
    }

    fn empty(kind: &'static str) -> Self {
        Self {
            kind,
            status: HookStatus::Empty,
            items: Some(0),
            omitted: None,
            metadata: Vec::new(),
            body: "no results".to_string(),
            segments: Vec::new(),
        }
    }

    fn segments(kind: &'static str, segments: Vec<HookResult>) -> Self {
        let status = if segments
            .iter()
            .all(|segment| segment.status == HookStatus::Empty)
        {
            HookStatus::Empty
        } else {
            HookStatus::Ok
        };
        Self {
            kind,
            status,
            items: Some(segments.len()),
            omitted: None,
            metadata: Vec::new(),
            body: String::new(),
            segments,
        }
    }

    fn render(&self) -> String {
        let mut output = Vec::new();
        output.push("slowcatch_result v1 text".to_string());
        output.push(self.header_line());
        output.extend(self.metadata.clone());
        if !self.segments.is_empty() {
            output.push(String::new());
            for (index, segment) in self.segments.iter().enumerate() {
                if index > 0 {
                    output.push(String::new());
                }
                output.push(segment.segment_header_line(index + 1));
                output.extend(segment.metadata.clone());
                if !segment.body.is_empty() {
                    if !segment.metadata.is_empty() {
                        output.push(String::new());
                    }
                    output.push(segment.body.clone());
                }
            }
        } else if !self.body.is_empty() {
            output.push(String::new());
            output.push(self.body.clone());
        }
        output.join("\n")
    }

    fn header_line(&self) -> String {
        let mut parts = vec![
            format!("kind={}", self.kind),
            format!("status={}", self.status.as_str()),
        ];
        if let Some(items) = self.items {
            parts.push(format!("items={items}"));
        }
        if let Some(omitted) = self.omitted {
            parts.push(format!("omitted={omitted}"));
        }
        parts.join(" ")
    }

    fn segment_header_line(&self, index: usize) -> String {
        format!("segment {index} {}", self.header_line())
    }
}

fn normalize_body_and_count_items(body: &str) -> (String, usize, Option<usize>) {
    let mut body_lines = Vec::new();
    let mut items = 0usize;
    let mut omitted = None;
    for line in body.lines() {
        if let Some(value) = line.strip_prefix("omitted_count=") {
            omitted = value.parse().ok();
            continue;
        }
        body_lines.push(line);
        if !line.trim().is_empty() && line != "--" {
            items += 1;
        }
    }
    (body_lines.join("\n"), items, omitted)
}

fn evaluate_condition(condition: &MiniCondition) -> Result<bool> {
    match condition {
        MiniCondition::TestPath { path, path_type } => {
            let metadata = match std::fs::metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error.into()),
            };
            Ok(match path_type {
                TestPathType::Any => true,
                TestPathType::Leaf => metadata.is_file(),
                TestPathType::Container => metadata.is_dir(),
            })
        }
        MiniCondition::Not(inner) => Ok(!evaluate_condition(inner)?),
        MiniCondition::And(left, right) => {
            Ok(evaluate_condition(left)? && evaluate_condition(right)?)
        }
        MiniCondition::Or(left, right) => {
            Ok(evaluate_condition(left)? || evaluate_condition(right)?)
        }
    }
}

fn execute_operation_with_timeout(
    operation: &FastOperation,
    finder: Arc<dyn FileFinder>,
    cwd: Option<&str>,
    timeout: Duration,
) -> Result<HookResult> {
    let operation = operation.clone();
    let cwd = cwd.map(ToOwned::to_owned);
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = execute_operation(&operation, finder.as_ref(), cwd.as_deref());
        let _ = sender.send(result);
    });
    receiver
        .recv_timeout(timeout)
        .map_err(|_| anyhow::anyhow!("hook operation timed out after {}ms", timeout.as_millis()))?
}

fn log_fast_path_failure(
    path: &std::path::Path,
    command: &str,
    operation: &FastOperation,
    context: &HookLogContext,
    error: &anyhow::Error,
) {
    let detail = UnknownCommand {
        reason: format!(
            "fast_path_backend_failure:{operation}: {error}",
            operation = operation_kind(operation)
        ),
        first_command: first_command_from_fast_command(command),
        pipeline_commands: pipeline_commands_from_fast_command(command),
        normalized_shape: format!("fast_path_failure:{}", operation_kind(operation)),
    };
    let _ = crate::unknown_log::append_unknown_command(path, command, &detail, context);
}

fn operation_kind(operation: &FastOperation) -> &'static str {
    match operation {
        FastOperation::Find(_) => "find",
        FastOperation::FindProjection(_) => "find_projection",
        FastOperation::FindPipeline(_) => "find_pipeline",
        FastOperation::FindMeasure(_) => "find_measure",
        FastOperation::DirectProjection { .. } => "direct_projection",
        FastOperation::Grep(_) => "grep",
        FastOperation::GrepContext(_) => "grep_context",
        FastOperation::InspectFile { .. } => "inspect_file",
        FastOperation::ReadLines { .. } => "read_lines",
        FastOperation::Slice { .. } => "slice",
        FastOperation::ListDirectory(_) => "list_directory",
        FastOperation::ListDirectoryPipeline(_) => "list_directory_pipeline",
        FastOperation::CommandList(_) => "command_list",
        FastOperation::MiniScript(_) => "mini_script",
    }
}

fn first_command_from_fast_command(command: &str) -> Option<String> {
    pipeline_commands_from_fast_command(command)
        .into_iter()
        .next()
}

fn pipeline_commands_from_fast_command(command: &str) -> Vec<String> {
    command
        .split('|')
        .filter_map(|part| part.split_whitespace().next())
        .map(|command| command.trim_matches('"').trim_matches('\'').to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::everything::FindQuery;
    use crate::integrations::project_check::ProjectCheckFailure;
    use std::path::PathBuf;

    struct MockFinder;

    impl FileFinder for MockFinder {
        fn find(&self, _query: &FindQuery) -> Result<Vec<String>> {
            Ok(vec!["C:\\repo\\main.rs".to_string()])
        }
    }

    struct MockProjectRunner {
        result: Option<ProjectCheckFailure>,
    }

    impl ProjectCommandRunner for MockProjectRunner {
        fn run(
            &self,
            _command: &str,
            _cwd: &Path,
            _timeout: Duration,
        ) -> Result<Option<ProjectCheckFailure>> {
            Ok(self.result.clone())
        }
    }

    #[test]
    fn hook_success_blocks_with_fast_path_output() {
        let input = r#"{"cwd":"C:\\repo","tool_input":{"command":"Get-ChildItem C:\\repo -Recurse -Filter *.rs"}}"#;
        let output = handle_hook_json(input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("\"decision\":\"block\""));
        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("C:\\\\repo\\\\main.rs"));
    }

    #[test]
    fn hook_unsupported_command_fails_open() {
        let input = r#"{"cwd":"C:\\repo","tool_input":{"command":"Get-Content file.txt"}}"#;
        let output = handle_hook_json(input, &MockFinder).unwrap();

        assert!(output.is_none());
    }

    #[test]
    fn hook_unknown_candidate_logs_and_fails_open() {
        let temp = tempfile::tempdir().unwrap();
        let log_path = temp.path().join("unknown.jsonl");
        let input = r#"{"session_id":"s1","turn_id":"t1","model":"m1","cwd":"C:\\repo","tool_input":{"command":"gci C:\\repo -Recurse -Filter *.rs | Where-Object Name -like '*main*' | Select-String needle"}}"#;

        let output = handle_hook_json_with_log_path(input, &MockFinder, &log_path).unwrap();

        assert!(output.is_none());
        let log = std::fs::read_to_string(log_path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
        assert_eq!(entry["source"], "codex_hook");
        assert_eq!(entry["session_id"], "s1");
        assert_eq!(entry["turn_id"], "t1");
        assert_eq!(entry["model"], "m1");
        assert!(entry["command"].as_str().unwrap().contains("Where-Object"));
        assert_eq!(entry["first_command"], "gci");
        assert!(
            entry["normalized_shape"]
                .as_str()
                .unwrap()
                .contains("where-object")
        );
    }

    #[test]
    fn hook_pass_through_commands_do_not_log() {
        let temp = tempfile::tempdir().unwrap();
        let log_path = temp.path().join("unknown.jsonl");

        for command in ["git status", "Remove-Item C:\\repo -Recurse"] {
            let input = format!(
                r#"{{"cwd":"C:\\repo","tool_input":{{"command":"{}"}}}}"#,
                command.replace('\\', "\\\\")
            );
            let output = handle_hook_json_with_log_path(&input, &MockFinder, &log_path).unwrap();
            assert!(output.is_none(), "{command}");
        }

        assert!(!log_path.exists());
    }

    #[test]
    fn hook_log_failure_still_fails_open() {
        let temp = tempfile::tempdir().unwrap();
        let directory_path = temp.path();
        let input = r#"{"cwd":"C:\\repo","tool_input":{"command":"Get-Content C:\\repo\\file.txt | Select-String -Pattern needle -Context 0,1"}}"#;

        let output = handle_hook_json_with_log_path(input, &MockFinder, directory_path).unwrap();

        assert!(output.is_none());
    }

    struct FailingFinder;

    impl FileFinder for FailingFinder {
        fn find(&self, _query: &FindQuery) -> Result<Vec<String>> {
            anyhow::bail!("backend unavailable")
        }
    }

    struct SlowFinder;

    impl FileFinder for SlowFinder {
        fn find(&self, _query: &FindQuery) -> Result<Vec<String>> {
            std::thread::sleep(std::time::Duration::from_secs(2));
            Ok(vec!["late".to_string()])
        }
    }

    #[test]
    fn hook_operation_timeout_logs_and_fails_open() {
        let temp = tempfile::tempdir().unwrap();
        let log_path = temp.path().join("unknown.jsonl");
        let input = r#"{"cwd":"C:\\repo","tool_input":{"command":"Get-ChildItem C:\\repo -Recurse -Filter *.rs"}}"#;
        let start = std::time::Instant::now();

        let output = handle_hook_json_with_log_path_and_timeout(
            input,
            Arc::new(SlowFinder),
            &log_path,
            std::time::Duration::from_millis(50),
        )
        .unwrap();

        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        assert!(output.is_none());
        let log = std::fs::read_to_string(log_path).unwrap();
        assert!(log.contains("hook operation timed out"));
    }

    #[test]
    fn hook_backend_failure_logs_and_fails_open() {
        let temp = tempfile::tempdir().unwrap();
        let log_path = temp.path().join("unknown.jsonl");
        let input = r#"{"cwd":"C:\\repo","tool_input":{"command":"Get-ChildItem C:\\repo -Recurse -Filter *.rs"}}"#;
        let output = handle_hook_json_with_log_path(input, &FailingFinder, &log_path).unwrap();

        assert!(output.is_none());
        let log = std::fs::read_to_string(log_path).unwrap();
        let entry: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
        assert_eq!(entry["first_command"], "get-childitem");
        assert!(
            entry["reason"]
                .as_str()
                .unwrap()
                .contains("fast_path_backend_failure:find")
        );
    }

    #[test]
    fn hook_inspect_file_blocks_with_numbered_output() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let input = format!(
            r#"{{"cwd":"{}","tool_input":{{"command":"Get-Content -Raw '{}'"}}}}"#,
            temp.path().display(),
            file.display()
        )
        .replace('\\', "\\\\");

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("slowcatch_result v1 text"));
        assert!(output.contains("kind=inspect_file status=ok"));
        assert!(output.contains("file_kind=code"));
        assert!(output.contains("render=full"));
        assert!(output.contains("line_numbers=original"));
        assert!(output.contains("line_format=source:L<n>| summary:L<n> <type>|"));
        assert!(output.contains("L1 | fn main() {}"));
    }

    #[test]
    fn hook_grep_context_blocks_with_context_output() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.txt");
        std::fs::write(&file, "before\nneedle\nnext\n").unwrap();
        let input = format!(
            r#"{{"cwd":"{}","tool_input":{{"command":"Select-String -Path '{}' -Pattern needle -Context 1,1 -SimpleMatch"}}}}"#,
            temp.path().display(),
            file.display()
        )
        .replace('\\', "\\\\");

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("L1:before: before"));
        assert!(output.contains("L2:match: needle"));
    }

    #[test]
    fn hook_get_content_totalcount_blocks_with_head_lines() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.txt");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let input = json!({
            "cwd": temp.path().display().to_string(),
            "tool_input": {
                "command": format!("Get-Content -LiteralPath '{}' -TotalCount 2", file.display())
            }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("one\\ntwo"));
        assert!(!output.contains("three"));
    }

    #[test]
    fn hook_get_content_tail_blocks_with_tail_lines() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("sample.txt");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let input = json!({
            "cwd": temp.path().display().to_string(),
            "tool_input": {
                "command": format!("Get-Content -Path '{}' -Tail 2 -ErrorAction SilentlyContinue", file.display())
            }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("two\\nthree"));
        assert!(!output.contains("one\\n"));
    }

    #[test]
    fn hook_missing_bounded_read_fails_open() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.txt");
        let input = json!({
            "cwd": temp.path().display().to_string(),
            "tool_input": {
                "command": format!("Get-Content -LiteralPath '{}' -TotalCount 2", missing.display())
            }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap();

        assert!(output.is_none());
    }

    #[test]
    fn hook_get_child_item_force_name_blocks_with_names() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("a.txt"), "a").unwrap();
        std::fs::write(temp.path().join("b.txt"), "b").unwrap();
        let input = json!({
            "cwd": temp.path().display().to_string(),
            "tool_input": { "command": "Get-ChildItem -Force -Name" }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("a.txt"));
        assert!(output.contains("b.txt"));
        assert!(!output.contains(&temp.path().display().to_string().replace('\\', "\\\\")));
    }

    #[test]
    fn hook_empty_directory_listing_uses_standard_empty_result() {
        let temp = tempfile::tempdir().unwrap();
        let input = json!({
            "cwd": temp.path().display().to_string(),
            "tool_input": { "command": "Get-ChildItem -Force -Filter '*.missing'" }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("slowcatch_result v1 text"));
        assert!(output.contains("kind=list_directory status=empty items=0"));
        assert!(output.contains("no results"));
    }

    #[test]
    fn hook_get_child_item_literalpath_filter_blocks_with_paths() {
        let temp = tempfile::tempdir().unwrap();
        let keep = temp.path().join("keep.uid");
        std::fs::write(&keep, "uid").unwrap();
        std::fs::write(temp.path().join("skip.txt"), "txt").unwrap();
        let input = json!({
            "cwd": "C:\\fallback",
            "tool_input": {
                "command": format!("Get-ChildItem -LiteralPath '{}' -Filter '*.uid'", temp.path().display())
            }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains(&keep.display().to_string().replace('\\', "\\\\")));
        assert!(!output.contains("skip.txt"));
    }

    #[test]
    fn hook_directory_match_pipeline_blocks_with_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let keep = temp.path().join("View3D");
        let skip = temp.path().join("Other");
        std::fs::create_dir(&keep).unwrap();
        std::fs::create_dir(&skip).unwrap();
        let input = json!({
            "cwd": "C:\\fallback",
            "tool_input": {
                "command": format!(
                    "Get-ChildItem -Path '{}' -Directory -ErrorAction SilentlyContinue | Where-Object {{ $_.Name -match 'View3D' }} | Select-Object FullName,LastWriteTime",
                    temp.path().display()
                )
            }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("FullName="));
        assert!(output.contains("View3D"));
        assert!(!output.contains("Other"));
    }

    #[test]
    fn hook_find_projection_blocks_with_projected_paths() {
        let input = r#"{"cwd":"C:\\repo","tool_input":{"command":"Get-ChildItem -Recurse -Filter *.rs | Select-Object -ExpandProperty FullName"}}"#;

        let output = handle_hook_json(input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("C:\\\\repo\\\\main.rs"));
    }

    #[test]
    fn hook_find_pipeline_filters_sorts_limits_and_projects() {
        struct PipelineFinder {
            paths: Vec<String>,
        }

        impl FileFinder for PipelineFinder {
            fn find(&self, _query: &FindQuery) -> Result<Vec<String>> {
                Ok(self.paths.clone())
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old.rs");
        let new = temp.path().join("new.rs");
        let note = temp.path().join("note.txt");
        std::fs::write(&old, "old").unwrap();
        std::fs::write(&new, "new file with more bytes").unwrap();
        std::fs::write(&note, "note").unwrap();
        let command = format!(
            "Get-ChildItem '{}' -Recurse -Filter * | Where-Object Name -like *.rs | Sort-Object Length -Descending | Select-Object -First 1 FullName",
            temp.path().display()
        );
        let input = json!({
            "cwd": temp.path().display().to_string(),
            "tool_input": { "command": command }
        })
        .to_string();

        let output = handle_hook_json(
            &input,
            &PipelineFinder {
                paths: vec![
                    old.display().to_string(),
                    note.display().to_string(),
                    new.display().to_string(),
                ],
            },
        )
        .unwrap()
        .unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains(&new.display().to_string().replace('\\', "\\\\")));
        assert!(!output.contains(&old.display().to_string().replace('\\', "\\\\")));
    }

    #[test]
    fn hook_find_measure_counts_filtered_paths() {
        struct MeasureFinder {
            paths: Vec<String>,
        }

        impl FileFinder for MeasureFinder {
            fn find(&self, _query: &FindQuery) -> Result<Vec<String>> {
                Ok(self.paths.clone())
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let rs = temp.path().join("main.rs");
        let txt = temp.path().join("note.txt");
        std::fs::write(&rs, "rust").unwrap();
        std::fs::write(&txt, "text").unwrap();
        let command = format!(
            "Get-ChildItem '{}' -Recurse | Where-Object Extension -eq .rs | Measure-Object",
            temp.path().display()
        );
        let input = json!({
            "cwd": temp.path().display().to_string(),
            "tool_input": { "command": command }
        })
        .to_string();

        let output = handle_hook_json(
            &input,
            &MeasureFinder {
                paths: vec![rs.display().to_string(), txt.display().to_string()],
            },
        )
        .unwrap()
        .unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("Count=1"));
    }

    #[test]
    fn hook_command_list_uses_compact_segment_headers() {
        let cwd = env!("CARGO_MANIFEST_DIR");
        let input = json!({
            "cwd": cwd,
            "tool_input": {
                "command": format!("Get-ChildItem -LiteralPath '{}' -Filter Cargo.toml; git log --oneline -n 1", cwd)
            }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("kind=command_list status=ok items=2"));
        assert!(output.contains("segment 1 kind=list_directory status=ok items=1"));
        assert!(output.contains("segment 2 kind=readonly_external status=ok"));
        assert!(!output.contains("### segment"));
    }

    #[test]
    fn hook_normal_output_does_not_include_debug_fields() {
        let input = r#"{"cwd":"C:\\repo","tool_input":{"command":"Get-ChildItem C:\\repo -Recurse -Filter *.rs"}}"#;
        let output = handle_hook_json(input, &MockFinder).unwrap().unwrap();

        for field in ["command=", "parser=", "backend=", "detector=", "debug=1"] {
            assert!(!output.contains(field), "{field} leaked into normal output");
        }
    }

    #[test]
    fn hook_user_prompt_submit_adds_uuid_lookup_context() {
        struct PromptFinder;

        impl FileFinder for PromptFinder {
            fn find(&self, _query: &FindQuery) -> Result<Vec<String>> {
                Ok(vec![
                    "C:\\repo\\a-019e0b7c-4f63-7bc1-8d24-0586a9098481.jsonl".to_string(),
                ])
            }
        }

        let input = json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "s1",
            "turn_id": "t1",
            "model": "m1",
            "cwd": "C:\\repo",
            "prompt": "please inspect 019e0b7c-4f63-7bc1-8d24-0586a9098481"
        })
        .to_string();

        let output = handle_hook_json(&input, &PromptFinder).unwrap().unwrap();

        assert!(output.contains("\"hookEventName\":\"UserPromptSubmit\""));
        assert!(output.contains("slowcatch_refs v1 paths_only"));
        assert!(output.contains("uuid:019e0b7c-4f63-7bc1-8d24-0586a9098481 [1/1]"));
        assert!(!output.contains("detector=uuid_path_lookup"));
        assert!(output.contains("C:\\\\repo\\\\a-019e0b7c-4f63-7bc1-8d24-0586a9098481.jsonl"));
    }

    #[test]
    fn hook_post_tool_use_apply_patch_clean_file_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("clean.rs");
        std::fs::write(&file, "fn main() {\n    println!(\"ok\");\n}\n").unwrap();
        let input = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "apply_patch",
            "cwd": temp.path().display().to_string(),
            "tool_input": {
                "command": format!("*** Begin Patch\n*** Update File: {}\n*** End Patch", file.display())
            },
            "tool_response": { "output": "Success. Updated the following files" }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap();

        assert!(output.is_none());
    }

    #[test]
    fn hook_post_tool_use_records_touched_paths() {
        let temp = tempfile::tempdir().unwrap();
        let state_path = temp.path().join("state.jsonl");
        let file = temp.path().join("clean.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let input = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "apply_patch",
            "session_id": "s1",
            "turn_id": "t1",
            "cwd": temp.path().display().to_string(),
            "tool_input": {
                "command": format!("*** Begin Patch\n*** Update File: {}\n*** End Patch", file.display())
            },
            "tool_response": { "output": "Success. Updated the following files" }
        })
        .to_string();

        let output = handle_hook_json_with_state_and_runner(
            &input,
            &MockFinder,
            &state_path,
            &MockProjectRunner { result: None },
        )
        .unwrap();

        assert!(output.is_none());
        let paths =
            post_edit_state::read_touched_paths(&state_path, Some("s1"), Some("t1")).unwrap();
        assert_eq!(paths, vec![file]);
    }

    #[test]
    fn hook_post_tool_use_apply_patch_with_conflict_marker_blocks() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("conflict.rs");
        std::fs::write(&file, "fn main() {\n<<<<<<< ours\n}\n").unwrap();
        let input = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "apply_patch",
            "cwd": temp.path().display().to_string(),
            "tool_input": {
                "command": format!("*** Begin Patch\n*** Update File: {}\n*** End Patch", file.display())
            },
            "tool_response": { "output": "Success. Updated the following files" }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("\"decision\":\"block\""));
        assert!(output.contains("\"hookEventName\":\"PostToolUse\""));
        assert!(output.contains("slowcatch_post_edit_check v1 issues"));
        assert!(output.contains("git conflict marker"));
    }

    #[test]
    fn hook_post_tool_use_apply_patch_failure_fails_open() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("conflict.rs");
        std::fs::write(&file, "fn main() {\n<<<<<<< ours\n}\n").unwrap();
        let input = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "apply_patch",
            "cwd": temp.path().display().to_string(),
            "tool_input": {
                "command": format!("*** Begin Patch\n*** Update File: {}\n*** End Patch", file.display())
            },
            "tool_response": { "error": "patch failed" }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap();

        assert!(output.is_none());
    }

    #[test]
    fn hook_stop_with_no_state_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        let input = json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
            "turn_id": "t1",
            "cwd": temp.path().display().to_string()
        })
        .to_string();

        let output = handle_hook_json_with_state_and_runner(
            &input,
            &MockFinder,
            &temp.path().join("missing.jsonl"),
            &MockProjectRunner { result: None },
        )
        .unwrap();

        assert!(output.is_none());
    }

    #[test]
    fn hook_stop_with_failing_project_check_blocks() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        std::fs::write(
            temp.path().join(".slowcatch.toml"),
            r#"[post_edit.project_check]
enabled = true
kind = "rust"
command = "cargo check -q"
timeout_seconds = 15
trigger = "stop"
changed_files = ["*.rs"]
"#,
        )
        .unwrap();
        let state_path = temp.path().join("state.jsonl");
        let touched = temp.path().join("src/lib.rs");
        post_edit_state::append_touched_paths(
            &state_path,
            Some("s1"),
            Some("t1"),
            Some(temp.path().to_str().unwrap()),
            &[touched],
        )
        .unwrap();
        let input = json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
            "turn_id": "t1",
            "cwd": temp.path().display().to_string()
        })
        .to_string();

        let output = handle_hook_json_with_state_and_runner(
            &input,
            &MockFinder,
            &state_path,
            &MockProjectRunner {
                result: Some(ProjectCheckFailure {
                    command: "cargo check -q".to_string(),
                    cwd: PathBuf::from(temp.path()),
                    exit_code: Some(101),
                    stdout: String::new(),
                    stderr: "error[E0425]: cannot find value".to_string(),
                }),
            },
        )
        .unwrap()
        .unwrap();

        assert!(output.contains("\"decision\":\"block\""));
        assert!(output.contains("slowcatch_project_check v1 failed"));
        assert!(output.contains("error[E0425]"));
    }

    #[test]
    fn hook_stop_with_passing_project_check_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        std::fs::write(
            temp.path().join(".slowcatch.toml"),
            r#"[post_edit.project_check]
enabled = true
kind = "rust"
"#,
        )
        .unwrap();
        let state_path = temp.path().join("state.jsonl");
        let touched = temp.path().join("src/lib.rs");
        post_edit_state::append_touched_paths(
            &state_path,
            Some("s1"),
            Some("t1"),
            Some(temp.path().to_str().unwrap()),
            &[touched],
        )
        .unwrap();
        let input = json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
            "turn_id": "t1",
            "cwd": temp.path().display().to_string()
        })
        .to_string();

        let output = handle_hook_json_with_state_and_runner(
            &input,
            &MockFinder,
            &state_path,
            &MockProjectRunner { result: None },
        )
        .unwrap();

        assert!(output.is_none());
    }

    #[test]
    fn hook_stop_with_failing_cpp_check_blocks() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("build")).unwrap();
        std::fs::write(temp.path().join("build/compile_commands.json"), "[]").unwrap();
        std::fs::write(
            temp.path().join("main.cpp"),
            "int main() { return missing; }\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join(".slowcatch.toml"),
            r#"[post_edit.cpp_check]
enabled = true
kind = "cpp"
command = "clangd --check={file} --compile-commands-dir={compile_commands_dir}"
timeout_seconds = 20
trigger = "stop"
changed_files = ["*.cpp"]
compile_commands_dir = "build"
"#,
        )
        .unwrap();
        let state_path = temp.path().join("state.jsonl");
        let touched = temp.path().join("main.cpp");
        post_edit_state::append_touched_paths(
            &state_path,
            Some("s1"),
            Some("t1"),
            Some(temp.path().to_str().unwrap()),
            &[touched],
        )
        .unwrap();
        let input = json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
            "turn_id": "t1",
            "cwd": temp.path().display().to_string()
        })
        .to_string();

        let output = handle_hook_json_with_state_and_runner(
            &input,
            &MockFinder,
            &state_path,
            &MockProjectRunner {
                result: Some(ProjectCheckFailure {
                    command: "clangd --check=main.cpp".to_string(),
                    cwd: PathBuf::from(temp.path()),
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: "use of undeclared identifier 'missing'".to_string(),
                }),
            },
        )
        .unwrap()
        .unwrap();

        assert!(output.contains("\"decision\":\"block\""));
        assert!(output.contains("slowcatch_project_check v1 failed"));
        assert!(output.contains("kind=cpp"));
        assert!(output.contains("main.cpp"));
        assert!(output.contains("undeclared identifier"));
    }

    #[test]
    fn hook_stop_with_passing_cpp_check_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("build")).unwrap();
        std::fs::write(temp.path().join("build/compile_commands.json"), "[]").unwrap();
        std::fs::write(temp.path().join("main.cpp"), "int main() { return 0; }\n").unwrap();
        std::fs::write(
            temp.path().join(".slowcatch.toml"),
            r#"[post_edit.cpp_check]
enabled = true
kind = "cpp"
compile_commands_dir = "build"
"#,
        )
        .unwrap();
        let state_path = temp.path().join("state.jsonl");
        let touched = temp.path().join("main.cpp");
        post_edit_state::append_touched_paths(
            &state_path,
            Some("s1"),
            Some("t1"),
            Some(temp.path().to_str().unwrap()),
            &[touched],
        )
        .unwrap();
        let input = json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
            "turn_id": "t1",
            "cwd": temp.path().display().to_string()
        })
        .to_string();

        let output = handle_hook_json_with_state_and_runner(
            &input,
            &MockFinder,
            &state_path,
            &MockProjectRunner { result: None },
        )
        .unwrap();

        assert!(output.is_none());
    }

    #[test]
    fn hook_stop_with_rust_and_cpp_failures_combines_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("build")).unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        std::fs::write(temp.path().join("build/compile_commands.json"), "[]").unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        std::fs::write(
            temp.path().join("main.cpp"),
            "int main() { return missing; }\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join(".slowcatch.toml"),
            r#"[post_edit.project_check]
enabled = true
kind = "rust"

[post_edit.cpp_check]
enabled = true
kind = "cpp"
compile_commands_dir = "build"
"#,
        )
        .unwrap();
        let state_path = temp.path().join("state.jsonl");
        post_edit_state::append_touched_paths(
            &state_path,
            Some("s1"),
            Some("t1"),
            Some(temp.path().to_str().unwrap()),
            &[temp.path().join("src/lib.rs"), temp.path().join("main.cpp")],
        )
        .unwrap();
        let input = json!({
            "hook_event_name": "Stop",
            "session_id": "s1",
            "turn_id": "t1",
            "cwd": temp.path().display().to_string()
        })
        .to_string();

        let output = handle_hook_json_with_state_and_runner(
            &input,
            &MockFinder,
            &state_path,
            &MockProjectRunner {
                result: Some(ProjectCheckFailure {
                    command: "mock check".to_string(),
                    cwd: PathBuf::from(temp.path()),
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: "diagnostic".to_string(),
                }),
            },
        )
        .unwrap()
        .unwrap();

        assert!(output.contains("check 1:"));
        assert!(output.contains("check 2:"));
        assert!(output.contains("kind=rust"));
        assert!(output.contains("kind=cpp"));
    }

    #[test]
    fn hook_mini_script_true_branch_blocks_with_fast_path_output() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("FLASHBACK.md");
        std::fs::write(&file, "# title\nline\n").unwrap();
        let command = format!(
            r#"if (Test-Path "{}") {{ Get-Content -Raw "{}" }}"#,
            file.display(),
            file.display()
        );
        let input = json!({
            "cwd": temp.path().display().to_string(),
            "tool_input": { "command": command }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("L1 | # title"));
    }

    #[test]
    fn hook_mini_script_false_branch_blocks_with_no_output() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.md");
        let command = format!(
            r#"if (Test-Path "{}") {{ Get-Content -Raw "{}" }}"#,
            missing.display(),
            missing.display()
        );
        let input = json!({
            "cwd": temp.path().display().to_string(),
            "tool_input": { "command": command }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("slowcatch_result v1 text"));
        assert!(output.contains("kind=mini_script status=empty items=0"));
        assert!(output.contains("no results"));
    }

    #[test]
    fn hook_mini_script_false_branch_runs_else_body() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.md");
        let fallback = temp.path().join("fallback.md");
        std::fs::write(&fallback, "fallback\n").unwrap();
        let command = format!(
            r#"if (Test-Path "{}") {{ Get-Content -Raw "{}" }} else {{ Get-Content -Raw "{}" }}"#,
            missing.display(),
            missing.display(),
            fallback.display()
        );
        let input = json!({
            "cwd": temp.path().display().to_string(),
            "tool_input": { "command": command }
        })
        .to_string();

        let output = handle_hook_json(&input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("L1 | fallback"));
    }

    #[test]
    fn hook_mini_script_backend_failure_logs_and_fails_open() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("dir.md");
        std::fs::create_dir(&directory).unwrap();
        let log_path = temp.path().join("unknown.jsonl");
        let command = format!(
            r#"if (Test-Path "{}") {{ Get-Content -Raw "{}" }}"#,
            directory.display(),
            directory.display()
        );
        let input = json!({
            "cwd": temp.path().display().to_string(),
            "tool_input": { "command": command }
        })
        .to_string();

        let output = handle_hook_json_with_log_path(&input, &MockFinder, &log_path).unwrap();

        assert!(output.is_none());
        let log = std::fs::read_to_string(log_path).unwrap();
        assert!(log.contains("fast_path_backend_failure:mini_script"));
    }
}
