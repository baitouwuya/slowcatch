use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::io::ErrorKind;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_CPP_CHECK_FILES: usize = 10;
const DEFAULT_CPP_COMMAND: &str =
    "clangd --check={file} --compile-commands-dir={compile_commands_dir}";
const DEFAULT_CPP_CHANGED_FILES: &[&str] = &[
    "*.c", "*.cc", "*.cpp", "*.cxx", "*.h", "*.hh", "*.hpp", "*.hxx",
];

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct SlowcatchConfig {
    post_edit: Option<PostEditConfig>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct PostEditConfig {
    project_check: Option<ProjectCheckConfig>,
    cpp_check: Option<CppCheckConfig>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ProjectCheckConfig {
    pub enabled: bool,
    pub kind: String,
    pub command: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub trigger: Option<String>,
    pub changed_files: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CppCheckConfig {
    pub enabled: bool,
    pub kind: String,
    pub command: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub trigger: Option<String>,
    pub changed_files: Option<Vec<String>>,
    pub compile_commands_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCheckRequest {
    pub cwd: PathBuf,
    pub touched_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCheckFailure {
    pub command: String,
    pub cwd: PathBuf,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCheckDiagnostic {
    pub kind: String,
    pub file: Option<PathBuf>,
    pub failure: ProjectCheckFailure,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectCheckPlan {
    Run {
        config: ProjectCheckConfig,
        cargo_root: PathBuf,
    },
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CppCheckPlan {
    config: CppCheckConfig,
    config_root: PathBuf,
    compile_commands_dir: PathBuf,
    files: Vec<PathBuf>,
}

pub trait ProjectCommandRunner {
    fn run(
        &self,
        command: &str,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<Option<ProjectCheckFailure>>;
}

pub struct ProcessProjectCommandRunner;

impl ProjectCommandRunner for ProcessProjectCommandRunner {
    fn run(
        &self,
        command: &str,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<Option<ProjectCheckFailure>> {
        run_project_command(command, cwd, timeout)
    }
}

#[cfg(test)]
pub fn run_project_check(
    request: ProjectCheckRequest,
    runner: &dyn ProjectCommandRunner,
) -> Result<Option<ProjectCheckFailure>> {
    match build_project_check_plan(&request)? {
        ProjectCheckPlan::Run { config, cargo_root } => {
            let command = config
                .command
                .as_deref()
                .unwrap_or("cargo check -q")
                .trim()
                .to_string();
            if command.is_empty() {
                return Ok(None);
            }
            let timeout = Duration::from_secs(config.timeout_seconds.unwrap_or(15).clamp(1, 120));
            runner.run(&command, &cargo_root, timeout)
        }
        ProjectCheckPlan::Skip => Ok(None),
    }
}

pub fn run_project_checks(
    request: ProjectCheckRequest,
    runner: &dyn ProjectCommandRunner,
) -> Result<Vec<ProjectCheckDiagnostic>> {
    let Some(config_path) = find_upward(&request.cwd, ".slowcatch.toml") else {
        return Ok(Vec::new());
    };
    let Some(config) = read_slowcatch_config(&config_path).unwrap_or(None) else {
        return Ok(Vec::new());
    };
    let mut diagnostics = Vec::new();
    if let Some(diagnostic) = run_rust_project_check(&request, &config, runner)? {
        diagnostics.push(diagnostic);
    }
    if let Some(plan) = build_cpp_check_plan(&request, &config, &config_path) {
        for file in plan.files.iter().take(MAX_CPP_CHECK_FILES) {
            let command_template = plan
                .config
                .command
                .as_deref()
                .unwrap_or(DEFAULT_CPP_COMMAND)
                .trim();
            if command_template.is_empty() {
                continue;
            }
            let command = render_cpp_command(command_template, file, &plan.compile_commands_dir);
            let timeout =
                Duration::from_secs(plan.config.timeout_seconds.unwrap_or(20).clamp(1, 120));
            if let Some(failure) = runner.run(&command, &plan.config_root, timeout)? {
                diagnostics.push(ProjectCheckDiagnostic {
                    kind: "cpp".to_string(),
                    file: Some(file.clone()),
                    failure,
                });
            }
        }
    }
    Ok(diagnostics)
}

#[cfg(test)]
pub fn render_project_check_failure(failure: &ProjectCheckFailure) -> String {
    render_failure_fields(None, None, failure)
}

pub fn render_project_check_failures(failures: &[ProjectCheckDiagnostic]) -> String {
    failures
        .iter()
        .enumerate()
        .map(|(index, diagnostic)| {
            let rendered = render_failure_fields(
                Some(diagnostic.kind.as_str()),
                diagnostic.file.as_deref(),
                &diagnostic.failure,
            );
            if failures.len() == 1 {
                rendered
            } else {
                format!("check {}:\n{rendered}", index + 1)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_failure_fields(
    kind: Option<&str>,
    file: Option<&Path>,
    failure: &ProjectCheckFailure,
) -> String {
    let mut lines = Vec::new();
    if let Some(kind) = kind {
        lines.push(format!("kind={kind}"));
    }
    if let Some(file) = file {
        lines.push(format!("file={}", file.display()));
    }
    lines.extend([
        format!("command={}", failure.command),
        format!("cwd={}", failure.cwd.display()),
        format!(
            "exit_code={}",
            failure
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
    ]);
    if !failure.stderr.trim().is_empty() {
        lines.push("stderr:".to_string());
        lines.push(trim_output(&failure.stderr));
    }
    if !failure.stdout.trim().is_empty() {
        lines.push("stdout:".to_string());
        lines.push(trim_output(&failure.stdout));
    }
    lines.join("\n")
}

fn run_rust_project_check(
    request: &ProjectCheckRequest,
    config: &SlowcatchConfig,
    runner: &dyn ProjectCommandRunner,
) -> Result<Option<ProjectCheckDiagnostic>> {
    let Some(config) = config
        .post_edit
        .as_ref()
        .and_then(|post| post.project_check.clone())
    else {
        return Ok(None);
    };
    if !config.enabled
        || config.kind != "rust"
        || config.trigger.as_deref().unwrap_or("stop") != "stop"
        || !changed_files_match(
            &request.touched_paths,
            config.changed_files.as_deref(),
            &["*.rs"],
        )
    {
        return Ok(None);
    }
    let Some(cargo_toml) = find_upward(&request.cwd, "Cargo.toml") else {
        return Ok(None);
    };
    let cargo_root = cargo_toml
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| request.cwd.clone());
    let command = config
        .command
        .as_deref()
        .unwrap_or("cargo check -q")
        .trim()
        .to_string();
    if command.is_empty() {
        return Ok(None);
    }
    let timeout = Duration::from_secs(config.timeout_seconds.unwrap_or(15).clamp(1, 120));
    Ok(runner
        .run(&command, &cargo_root, timeout)?
        .map(|failure| ProjectCheckDiagnostic {
            kind: "rust".to_string(),
            file: None,
            failure,
        }))
}

#[cfg(test)]
fn build_project_check_plan(request: &ProjectCheckRequest) -> Result<ProjectCheckPlan> {
    if !request
        .touched_paths
        .iter()
        .any(|path| path.extension().and_then(|value| value.to_str()) == Some("rs"))
    {
        return Ok(ProjectCheckPlan::Skip);
    }

    let Some(config_path) = find_upward(&request.cwd, ".slowcatch.toml") else {
        return Ok(ProjectCheckPlan::Skip);
    };
    let config = match read_config(&config_path) {
        Ok(Some(config)) => config,
        Ok(None) | Err(_) => return Ok(ProjectCheckPlan::Skip),
    };
    if !config.enabled
        || config.kind != "rust"
        || config.trigger.as_deref().unwrap_or("stop") != "stop"
        || !changed_files_match(
            &request.touched_paths,
            config.changed_files.as_deref(),
            &["*.rs"],
        )
    {
        return Ok(ProjectCheckPlan::Skip);
    }

    let Some(cargo_toml) = find_upward(&request.cwd, "Cargo.toml") else {
        return Ok(ProjectCheckPlan::Skip);
    };
    let cargo_root = cargo_toml
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| request.cwd.clone());
    Ok(ProjectCheckPlan::Run { config, cargo_root })
}

#[cfg(test)]
fn read_config(path: &Path) -> Result<Option<ProjectCheckConfig>> {
    Ok(read_slowcatch_config(path)?
        .and_then(|config| config.post_edit)
        .and_then(|post| post.project_check))
}

fn read_slowcatch_config(path: &Path) -> Result<Option<SlowcatchConfig>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
    let config: SlowcatchConfig = toml::from_str(&text)?;
    Ok(Some(config))
}

fn build_cpp_check_plan(
    request: &ProjectCheckRequest,
    config: &SlowcatchConfig,
    config_path: &Path,
) -> Option<CppCheckPlan> {
    let config = config
        .post_edit
        .as_ref()
        .and_then(|post| post.cpp_check.clone())?;
    if !config.enabled
        || config.kind != "cpp"
        || config.trigger.as_deref().unwrap_or("stop") != "stop"
        || !changed_files_match(
            &request.touched_paths,
            config.changed_files.as_deref(),
            DEFAULT_CPP_CHANGED_FILES,
        )
    {
        return None;
    }
    let config_root = config_path.parent()?.to_path_buf();
    let compile_commands_dir = resolve_config_path(
        &config_root,
        config.compile_commands_dir.as_deref().unwrap_or("."),
    );
    if !compile_commands_dir.join("compile_commands.json").is_file() {
        return None;
    }
    let files = request
        .touched_paths
        .iter()
        .map(|path| absolute_path(&request.cwd, path))
        .filter(|path| path.is_file())
        .filter(|path| {
            path_matches_patterns(
                path,
                config.changed_files.as_deref(),
                DEFAULT_CPP_CHANGED_FILES,
            )
        })
        .collect::<Vec<_>>();
    if files.is_empty() {
        return None;
    }
    Some(CppCheckPlan {
        config,
        config_root,
        compile_commands_dir,
        files,
    })
}

fn changed_files_match(
    paths: &[PathBuf],
    configured_patterns: Option<&[String]>,
    default_patterns: &[&str],
) -> bool {
    paths
        .iter()
        .any(|path| path_matches_patterns(path, configured_patterns, default_patterns))
}

fn path_matches_patterns(
    path: &Path,
    configured_patterns: Option<&[String]>,
    default_patterns: &[&str],
) -> bool {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if let Some(patterns) = configured_patterns {
        patterns
            .iter()
            .any(|pattern| wildcard_match(file_name, pattern))
    } else {
        default_patterns
            .iter()
            .any(|pattern| wildcard_match(file_name, pattern))
    }
}

fn wildcard_match(value: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(extension) = pattern.strip_prefix("*.") {
        return value
            .rsplit_once('.')
            .is_some_and(|(_, value_extension)| value_extension.eq_ignore_ascii_case(extension));
    }
    value.eq_ignore_ascii_case(pattern)
}

fn find_upward(start: &Path, file_name: &str) -> Option<PathBuf> {
    let mut current = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        let candidate = current.join(file_name);
        if candidate.exists() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

fn absolute_path(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn resolve_config_path(root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn render_cpp_command(template: &str, file: &Path, compile_commands_dir: &Path) -> String {
    template
        .replace("{file}", &quote_command_value(&file.display().to_string()))
        .replace(
            "{compile_commands_dir}",
            &quote_command_value(&compile_commands_dir.display().to_string()),
        )
}

fn quote_command_value(value: &str) -> String {
    if value.chars().any(char::is_whitespace) {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

fn run_project_command(
    command: &str,
    cwd: &Path,
    timeout: Duration,
) -> Result<Option<ProjectCheckFailure>> {
    let (program, args) = split_command(command)?;
    let mut child = match Command::new(&program)
        .args(&args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            if let Some(mut handle) = child.stdout.take() {
                handle.read_to_end(&mut stdout)?;
            }
            if let Some(mut handle) = child.stderr.take() {
                handle.read_to_end(&mut stderr)?;
            }
            if status.success() {
                return Ok(None);
            }
            return Ok(Some(ProjectCheckFailure {
                command: command.to_string(),
                cwd: cwd.to_path_buf(),
                exit_code: status.code(),
                stdout: capped_utf8(stdout),
                stderr: capped_utf8(stderr),
            }));
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn split_command(command: &str) -> Result<(String, Vec<String>)> {
    let parts = split_command_parts(command);
    let Some((program, args)) = parts.split_first() else {
        bail!("empty project check command");
    };
    Ok((program.clone(), args.to_vec()))
}

fn split_command_parts(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in command.chars() {
        match (quote, ch) {
            (Some(active), value) if value == active => quote = None,
            (None, '"' | '\'') => quote = Some(ch),
            (None, value) if value.is_whitespace() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn capped_utf8(mut bytes: Vec<u8>) -> String {
    if bytes.len() > MAX_OUTPUT_BYTES {
        bytes.truncate(MAX_OUTPUT_BYTES);
    }
    String::from_utf8_lossy(&bytes).to_string()
}

fn trim_output(output: &str) -> String {
    let mut text = output.trim().to_string();
    if text.len() > MAX_OUTPUT_BYTES {
        text.truncate(MAX_OUTPUT_BYTES);
        text.push_str("\n... truncated ...");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct MockRunner {
        result: Option<ProjectCheckFailure>,
        calls: RefCell<Vec<String>>,
    }

    impl MockRunner {
        fn new(result: Option<ProjectCheckFailure>) -> Self {
            Self {
                result,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl ProjectCommandRunner for MockRunner {
        fn run(
            &self,
            command: &str,
            _cwd: &Path,
            _timeout: Duration,
        ) -> Result<Option<ProjectCheckFailure>> {
            self.calls.borrow_mut().push(command.to_string());
            Ok(self.result.clone())
        }
    }

    fn write_rust_config(root: &Path) {
        std::fs::write(
            root.join(".slowcatch.toml"),
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
    }

    fn write_cpp_config(root: &Path) {
        std::fs::write(
            root.join(".slowcatch.toml"),
            r#"[post_edit.cpp_check]
enabled = true
kind = "cpp"
command = "clangd --check={file} --compile-commands-dir={compile_commands_dir}"
timeout_seconds = 20
trigger = "stop"
changed_files = ["*.c", "*.cc", "*.cpp", "*.cxx", "*.h", "*.hh", "*.hpp", "*.hxx"]
compile_commands_dir = "build"
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("build/compile_commands.json"), "[]").unwrap();
    }

    #[test]
    fn config_parser_accepts_documented_toml() {
        let temp = tempfile::tempdir().unwrap();
        write_rust_config(temp.path());

        let config = read_config(&temp.path().join(".slowcatch.toml"))
            .unwrap()
            .unwrap();

        assert!(config.enabled);
        assert_eq!(config.kind, "rust");
        assert_eq!(config.command.as_deref(), Some("cargo check -q"));
    }

    #[test]
    fn cpp_config_parser_accepts_documented_toml() {
        let temp = tempfile::tempdir().unwrap();
        write_cpp_config(temp.path());

        let config = read_slowcatch_config(&temp.path().join(".slowcatch.toml"))
            .unwrap()
            .unwrap()
            .post_edit
            .unwrap()
            .cpp_check
            .unwrap();

        assert!(config.enabled);
        assert_eq!(config.kind, "cpp");
        assert_eq!(config.timeout_seconds, Some(20));
        assert_eq!(config.compile_commands_dir.as_deref(), Some("build"));
    }

    #[test]
    fn project_check_skips_without_config_rs_change_or_cargo() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        let request = ProjectCheckRequest {
            cwd: temp.path().to_path_buf(),
            touched_paths: vec![temp.path().join("src/lib.rs")],
        };
        assert!(matches!(
            build_project_check_plan(&request).unwrap(),
            ProjectCheckPlan::Skip
        ));

        write_rust_config(temp.path());
        let request = ProjectCheckRequest {
            cwd: temp.path().to_path_buf(),
            touched_paths: vec![temp.path().join("README.md")],
        };
        assert!(matches!(
            build_project_check_plan(&request).unwrap(),
            ProjectCheckPlan::Skip
        ));

        let no_cargo = tempfile::tempdir().unwrap();
        write_rust_config(no_cargo.path());
        let request = ProjectCheckRequest {
            cwd: no_cargo.path().to_path_buf(),
            touched_paths: vec![no_cargo.path().join("lib.rs")],
        };
        assert!(matches!(
            build_project_check_plan(&request).unwrap(),
            ProjectCheckPlan::Skip
        ));
    }

    #[test]
    fn cpp_check_skips_without_cpp_change_or_compile_database() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".slowcatch.toml"),
            r#"[post_edit.cpp_check]
enabled = true
kind = "cpp"
"#,
        )
        .unwrap();
        let config = read_slowcatch_config(&temp.path().join(".slowcatch.toml"))
            .unwrap()
            .unwrap();
        let request = ProjectCheckRequest {
            cwd: temp.path().to_path_buf(),
            touched_paths: vec![temp.path().join("README.md")],
        };
        assert!(
            build_cpp_check_plan(&request, &config, &temp.path().join(".slowcatch.toml")).is_none()
        );

        let request = ProjectCheckRequest {
            cwd: temp.path().to_path_buf(),
            touched_paths: vec![temp.path().join("main.cpp")],
        };
        assert!(
            build_cpp_check_plan(&request, &config, &temp.path().join(".slowcatch.toml")).is_none()
        );
    }

    #[test]
    fn cpp_check_resolves_compile_commands_dir_relative_to_config() {
        let temp = tempfile::tempdir().unwrap();
        write_cpp_config(temp.path());
        let source = temp.path().join("main.cpp");
        std::fs::write(&source, "int main() { return 0; }\n").unwrap();
        let config = read_slowcatch_config(&temp.path().join(".slowcatch.toml"))
            .unwrap()
            .unwrap();
        let request = ProjectCheckRequest {
            cwd: temp.path().to_path_buf(),
            touched_paths: vec![source.clone()],
        };

        let plan =
            build_cpp_check_plan(&request, &config, &temp.path().join(".slowcatch.toml")).unwrap();

        assert_eq!(plan.compile_commands_dir, temp.path().join("build"));
        assert_eq!(plan.files, vec![source]);
    }

    #[test]
    fn cpp_check_caps_per_file_runs() {
        let temp = tempfile::tempdir().unwrap();
        write_cpp_config(temp.path());
        let mut touched_paths = Vec::new();
        for index in 0..12 {
            let path = temp.path().join(format!("file{index}.cpp"));
            std::fs::write(&path, "int value;\n").unwrap();
            touched_paths.push(path);
        }
        let runner = MockRunner::new(None);

        let result = run_project_checks(
            ProjectCheckRequest {
                cwd: temp.path().to_path_buf(),
                touched_paths,
            },
            &runner,
        )
        .unwrap();

        assert!(result.is_empty());
        assert_eq!(runner.calls.borrow().len(), MAX_CPP_CHECK_FILES);
    }

    #[test]
    fn failing_runner_returns_failure() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        write_rust_config(temp.path());

        let result = run_project_check(
            ProjectCheckRequest {
                cwd: temp.path().to_path_buf(),
                touched_paths: vec![temp.path().join("src/lib.rs")],
            },
            &MockRunner::new(Some(ProjectCheckFailure {
                command: "cargo check -q".to_string(),
                cwd: temp.path().to_path_buf(),
                exit_code: Some(101),
                stdout: String::new(),
                stderr: "error[E0425]: cannot find value".to_string(),
            })),
        )
        .unwrap()
        .unwrap();

        let rendered = render_project_check_failure(&result);
        assert!(rendered.contains("exit_code=101"));
        assert!(rendered.contains("error[E0425]"));
    }

    #[test]
    fn passing_runner_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        write_rust_config(temp.path());

        let result = run_project_check(
            ProjectCheckRequest {
                cwd: temp.path().to_path_buf(),
                touched_paths: vec![temp.path().join("src/lib.rs")],
            },
            &MockRunner::new(None),
        )
        .unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn missing_executable_and_timeout_fail_open() {
        let missing = run_project_command(
            "definitely_missing_slowcatch_command",
            Path::new("."),
            Duration::from_millis(50),
        )
        .unwrap();
        assert!(missing.is_none());

        let timeout =
            run_project_command("cargo --version", Path::new("."), Duration::from_nanos(1));
        assert!(timeout.unwrap().is_none());
    }
}
