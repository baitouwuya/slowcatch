use crate::backends::everything::{FileFinder, TimedEverythingFinder};
use crate::backends::grep;
use crate::backends::inspect_file;
use crate::backends::projection;
use crate::backends::slice;
use crate::shell_parse::{
    FastOperation, FastSegment, ParseDecision, UnknownCommand, classify_powershell,
};
use crate::unknown_log::HookLogContext;
use anyhow::Result;
use serde::Deserialize;
use serde_json::json;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

const FAST_PATH_PREFIX: &str = "FAST_PATH_SUCCESS";
const HOOK_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const EVERYTHING_HOOK_TIMEOUT: Duration = Duration::from_millis(1_500);

#[derive(Debug, Deserialize)]
struct HookInput {
    session_id: Option<String>,
    turn_id: Option<String>,
    model: Option<String>,
    cwd: Option<String>,
    tool_input: Option<ToolInput>,
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
    handle_hook_json_inner(input, unknown_log_path, |operation, cwd| {
        execute_operation(operation, finder, cwd)
    })
}

pub fn handle_hook_json_with_log_path_and_timeout(
    input: &str,
    finder: Arc<dyn FileFinder>,
    unknown_log_path: &std::path::Path,
    timeout: Duration,
) -> Result<Option<String>> {
    handle_hook_json_inner(input, unknown_log_path, |operation, cwd| {
        execute_operation_with_timeout(operation, Arc::clone(&finder), cwd, timeout)
    })
}

fn handle_hook_json_inner<F>(
    input: &str,
    unknown_log_path: &std::path::Path,
    execute: F,
) -> Result<Option<String>>
where
    F: Fn(&FastOperation, Option<&str>) -> Result<String>,
{
    let _ = unknown_log_path;
    let hook_input: HookInput = serde_json::from_str(input)?;
    let context = log_context(&hook_input);
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
                        "{FAST_PATH_PREFIX}: executed slowcatch substitute output; do not retry the original command.\n\n{output}"
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
) -> Result<String> {
    match operation {
        FastOperation::Find(query) => Ok(finder.find(query)?.join("\n")),
        FastOperation::FindProjection(projection) => {
            let paths = finder.find(&projection.query)?;
            projection::render_projection(&paths, &projection.projection)
        }
        FastOperation::Grep(query) => Ok(grep::grep(&query)?
            .into_iter()
            .map(|item| item.to_tool_text())
            .collect::<Vec<_>>()
            .join("\n")),
        FastOperation::GrepContext(query) => grep::grep_context(query),
        FastOperation::InspectFile { path, raw } => inspect_file::inspect_file(path, *raw),
        FastOperation::Slice { path, skip, first } => {
            Ok(slice::slice_lines(path, *skip, *first)?.join("\n"))
        }
        FastOperation::CommandList(segments) => {
            let mut output = Vec::new();
            for (index, segment) in segments.iter().enumerate() {
                output.push(format!("### segment {}", index + 1));
                match segment {
                    FastSegment::Operation(operation) => {
                        output.push(execute_operation(operation, finder, cwd)?);
                    }
                    FastSegment::ReadOnlyExternal(external) => {
                        output.push(projection::run_read_only_external(external, cwd)?);
                    }
                }
            }
            Ok(output.join("\n"))
        }
    }
}

fn execute_operation_with_timeout(
    operation: &FastOperation,
    finder: Arc<dyn FileFinder>,
    cwd: Option<&str>,
    timeout: Duration,
) -> Result<String> {
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
        FastOperation::Grep(_) => "grep",
        FastOperation::GrepContext(_) => "grep_context",
        FastOperation::InspectFile { .. } => "inspect_file",
        FastOperation::Slice { .. } => "slice",
        FastOperation::CommandList(_) => "command_list",
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

    struct MockFinder;

    impl FileFinder for MockFinder {
        fn find(&self, _query: &FindQuery) -> Result<Vec<String>> {
            Ok(vec!["C:\\repo\\main.rs".to_string()])
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
        assert!(output.contains("mode=full"));
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
    fn hook_find_projection_blocks_with_projected_paths() {
        let input = r#"{"cwd":"C:\\repo","tool_input":{"command":"Get-ChildItem -Recurse -Filter *.rs | Select-Object -ExpandProperty FullName"}}"#;

        let output = handle_hook_json(input, &MockFinder).unwrap().unwrap();

        assert!(output.contains("FAST_PATH_SUCCESS"));
        assert!(output.contains("C:\\\\repo\\\\main.rs"));
    }
}
