# slowcatch

Catch slow shell paths and replace them with safe fast paths.

`slowcatch` is a Windows-first Rust utility for local developer workflows. It
can run as a normal CLI, or as a Codex hook that accelerates narrow, read-only
PowerShell file operations and adds lightweight prompt lookups while failing
open to the original shell when anything is uncertain.

[中文文档](README.zh-CN.md)

## Install For Codex

Copy this into PowerShell. It installs the latest GitHub Release, configures the
Codex hook, and does not require cloning this repository.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "iwr https://raw.githubusercontent.com/baitouwuya/slowcatch/master/scripts/install-codex-hook.ps1 -UseB | iex"
```

Run the same command again to update.

To pin a version:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "& ([scriptblock]::Create((iwr https://raw.githubusercontent.com/baitouwuya/slowcatch/master/scripts/install-codex-hook.ps1 -UseB).Content)) -Version v0.1.3"
```

The installer downloads `slowcatch-x86_64-pc-windows-msvc.exe` from the latest
release, saves it as:

```text
%USERPROFILE%\.codex\bin\slowcatch.exe
```

It then creates or merges:

```text
%USERPROFILE%\.codex\hooks.json
```

The script preserves unrelated hooks and replaces only existing `slowcatch hook
codex` entries. It also recognizes older `rust-fast-tool hook codex` entries
and replaces them during migration. It installs `PreToolUse`,
`UserPromptSubmit`, `PostToolUse`, and `Stop` integrations. It does not silently edit `config.toml`. If
Codex hooks are not enabled, it prints the exact snippet to add:

```toml
[features]
codex_hooks = true
```

Restart Codex or reload hooks after installing if your current session does not
pick up hook changes automatically.

If the target executable is locked, the installer keeps going by placing a
versioned `slowcatch-<version>.exe` next to it and updating the hook to point at
that path.

Codex reads hook configuration at session scope. If you have an existing Codex
window open, restart Codex or reload hooks after updating so the session stops
using any previously loaded hook command.

### Repair Hooks JSON

If Codex reports:

```text
failed to parse hooks config ... hooks.json: expected value at line 1 column 1
```

run the install command again from PowerShell. The installer rewrites
`hooks.json` as UTF-8 without BOM. If the existing file is invalid JSON, it is
backed up as `hooks.json.bak.<timestamp>` and a fresh hook config is created.

## What It Does

`slowcatch` targets common slow, read-only shell patterns:

- filename search through Everything on Windows
- content search through Rust grep libraries
- streaming file line slicing
- optimized `Get-Content` inspection
- projected `Get-ChildItem | Select-Object` listings
- prompt UUID lookup through Everything path/name search
- structured CLI output for downstream tools
- safe Codex hook integration with shell fallback

The hook only replaces commands that match a strict whitelist. Mutating
commands, ambiguous syntax, dynamic PowerShell, external programs, and
unsupported shapes fall back to the original shell.

When a prompt contains a UUID-like value, `slowcatch` can look for related
files by path or file name and add a short `additionalContext` block before the
model continues.

## CLI Usage

Search file names:

```powershell
slowcatch find Cargo.toml --root E:\GitHub --limit 20
```

Search file contents:

```powershell
slowcatch grep "FAST_PATH_SUCCESS" E:\GitHub\slowcatch --limit 50
```

Read a line window without loading the whole file into PowerShell objects:

```powershell
slowcatch slice E:\GitHub\slowcatch\src\shell_parse\mod.rs --skip 200 --first 80
```

Check local parser and backend availability:

```powershell
slowcatch self-test
```

## Structured Output

The `find`, `grep`, and `slice` commands support:

```text
--output text|json|jsonl
--select column1,column2
--where column=value
--where column~substring
--sort-by column
--limit N
```

Examples:

```powershell
slowcatch find *.rs --root E:\GitHub\slowcatch --output jsonl --select path,extension --where extension=.rs
slowcatch grep "ParseDecision" E:\GitHub\slowcatch --output json --select path,line_number,line --sort-by path --limit 20
slowcatch slice README.md --skip 0 --first 30 --output jsonl
```

Available record fields:

- `find`: `path`, `name`, `extension`, `parent`
- `grep`: `path`, `name`, `extension`, `parent`, `line_number`, `line`
- `slice`: `line_number`, `text`

## Codex Hook Fast Paths

Install mode configures:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "^Bash$",
        "hooks": [
          {
            "type": "command",
            "command": "C:\\Users\\you\\.codex\\bin\\slowcatch.exe hook codex",
            "timeout": 15,
            "statusMessage": "Checking slowcatch fast-path"
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "C:\\Users\\you\\.codex\\bin\\slowcatch.exe hook codex",
            "timeout": 10,
            "statusMessage": "Looking up prompt references"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "^apply_patch$|^Edit$|^Write$",
        "hooks": [
          {
            "type": "command",
            "command": "C:\\Users\\you\\.codex\\bin\\slowcatch.exe hook codex",
            "timeout": 10,
            "statusMessage": "Checking edited files"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "C:\\Users\\you\\.codex\\bin\\slowcatch.exe hook codex",
            "timeout": 25,
            "statusMessage": "Checking project diagnostics"
          }
        ]
      }
    ]
  }
}
```

Supported PowerShell shapes include:

- `Get-ChildItem/gci/dir/ls ... -Recurse ... -Filter/-Include ...`
- `Get-ChildItem ... | Select-Object -ExpandProperty FullName`
- `Get-ChildItem ... | Select-Object FullName,Length,LastWriteTime,Mode,Name`
- `Select-String -Path ... -Pattern ...`
- `Select-String -Path ... -Pattern ... -Context A,B`
- `Get-ChildItem ... -Recurse | Select-String ...`
- `Get-Content file | Select-String ... -Context A,B`
- `Get-Content file | Select-Object -Skip N -First M`
- simple `Get-Content`, `gc`, `cat`, and `type` full-file inspection
- safe top-level `;` command lists where every segment is a known read-only
  fast path or a small allowlisted git status command

On success, the hook returns a Codex block decision whose reason starts with:

```text
FAST_PATH_SUCCESS
```

That message contains the substitute output and tells Codex not to retry the
original command. Fast-path output starts with a compact line-oriented header:

```text
slowcatch_result v1 text
kind=<operation_kind> status=<ok|empty> items=<n>

<body>
```

For command lists and mini-scripts, the body uses compact segment headers such
as `segment 1 kind=list_directory status=ok items=3`. Empty successful results
use `status=empty items=0` followed by `no results`.

On uncertainty or backend failure, the hook writes no stdout and exits `0`, so
Codex runs the original shell command unchanged.

After `apply_patch` edits, the `PostToolUse` hook stays silent when changed
files look clean. If it finds high-confidence issues such as conflict markers,
leftover patch markers, or obvious placeholder/debug code, it returns
`slowcatch_post_edit_check v1 issues` so Codex can immediately fix the files.

Post-edit quick checks are intentionally local and conservative. They read only
the changed files, cap work per turn, and fail open for missing, binary,
non-UTF-8, large, directory, unreadable, or unknown files. Findings currently
include:

- errors for Git conflict markers, leftover patch markers, and explicit
  removal placeholders such as `TODO&#95;REMOVE` and `FIXME&#95;REMOVE`
- errors for clear delimiter imbalance in brace-oriented languages
- warnings for Rust debug placeholders: `dbg!`, `todo!()`, and
  `unimplemented!()`
- warnings for lightweight tree-sitter syntax parse issues in Rust, Python, C,
  C++/headers, and GDScript

The quick checker is language-aware before applying rules. It masks comments,
strings, Markdown fenced code blocks, and other non-code regions where possible
to reduce false positives. Marker checks cover the supported code/config/text
paths, including Rust, Python, JavaScript/TypeScript, JSX/TSX, C/C++ headers,
Go, Java, C#, GDScript, JSON, TOML, YAML, CSS/SCSS, HTML/XML, Markdown/MDX,
Dockerfile, Makefile, `.env*`, `.gitignore`, `.gitattributes`, and
`.editorconfig`. This is not a compiler or LSP replacement; project diagnostics
belong to the Stop-time checks below.

For opt-in Rust project diagnostics, create `.slowcatch.toml` at the Cargo
project root:

```toml
[post_edit.project_check]
enabled = true
kind = "rust"
command = "cargo check -q"
timeout_seconds = 15
trigger = "stop"
changed_files = ["*.rs"]
```

When a turn edits Rust files, the `Stop` hook runs the configured command once.
Successful checks, timeouts, missing tools, missing config, and unrelated edits
stay silent. Real diagnostics return `slowcatch_project_check v1 failed` so
Codex continues with the build output.

For opt-in C++ diagnostics, create `.slowcatch.toml` next to an existing
`compile_commands.json` or point at its directory:

```toml
[post_edit.cpp_check]
enabled = true
kind = "cpp"
command = "clangd --check={file} --compile-commands-dir={compile_commands_dir}"
timeout_seconds = 20
trigger = "stop"
changed_files = ["*.c", "*.cc", "*.cpp", "*.cxx", "*.h", "*.hh", "*.hpp", "*.hxx"]
compile_commands_dir = "."
```

When a turn edits C or C++ files, the `Stop` hook runs `clangd --check` for the
changed files, capped to a small batch. Slowcatch requires an existing
`compile_commands.json`; it does not run CMake or generate build files. Missing
`clangd`, missing compile databases, timeouts, and unrelated edits stay silent.

For prompt lookups, a UUID-like token in the user message may trigger a small
Everything search. The hook only injects path names, not file contents, and
keeps output intentionally short:

```text
slowcatch_refs v1 paths_only
uuid:019e0b7c-4f63-7bc1-8d24-0586a9098481 [1/1]
C:\Users\you\.codex\sessions\rollout-019e0b7c-4f63-7bc1-8d24-0586a9098481.jsonl
```

Set `SLOWCATCH_PROMPT_DEBUG=1` to include detector diagnostics in the injected
context while troubleshooting prompt lookup behavior.

Hook fast paths also have an internal operation budget. Slow backends time out
inside `slowcatch`, are logged as backend failures, and fail open before Codex's
own hook timeout should fire.

## File Inspection

For simple `Get-Content` fast paths, the hook optimizes output by file type and
size:

- small code files: full line-numbered output
- small config and Markdown files: full line-numbered output with larger limits
- large code files: tree-sitter outline for Rust, Python, C, C++, headers, and
  GDScript
- large Markdown files: heading and fenced-code-block outline
- large config and lock files: top-level key or section summary
- binary, directory, missing, unreadable, or non-UTF-8 files: fail open to the
  original shell

File inspection output includes compact metadata:

```text
kind=inspect_file status=ok
file_path=<path>
file_kind=<code|markdown|config|lockfile|text|unknown>
render=<full|outline|summary|preview>
bytes=<n> lines=<n> line_numbers=original
line_format=source:L<n>| summary:L<n> <type>|
```

Full source output keeps original file order and uses token-efficient edit
anchors:

```text
L123 | content
```

Generated summaries use typed records:

```text
L18 symbol | function parse_powershell range=L18-L30
L42 heading | h2 File Inspection
L50 fence | range=L50-L54 lang=powershell
L7 key | dependencies
```

## Safety Model

The hook classifier has three outcomes:

- `Fast`: safe whitelist match; execute the Rust backend and substitute output
- `PassThrough`: known non-candidate, mutating command, risky syntax, or
  external program; do nothing and let the shell run
- `UnknownCandidate`: PowerShell file/content processing shape that may be
  optimizable later; log it and let the shell run

Unknown candidates and fast-path backend failures are appended as JSONL to:

```text
%USERPROFILE%\.codex\logs\slowcatch-unknown.jsonl
```

Logging failure is ignored. It never blocks the original shell fallback.

External programs are not broadly intercepted. Inside safe command lists, only
these read-only git forms may be executed directly:

- `git status --short`
- `git status --porcelain`
- `git status --porcelain=v1`
- `git status --porcelain=v2`
- `git diff --stat`
- `git log --oneline -n N` where `N` is `1..=50`

## Build From Source

```powershell
git clone https://github.com/baitouwuya/slowcatch.git
cd slowcatch
cargo test
cargo build --release
```

Optional live Everything validation:

```powershell
cargo test --features live-everything
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
