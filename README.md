# slowcatch

Catch slow shell paths and replace them with safe fast paths.

`slowcatch` is a Windows-first Rust utility for local developer workflows. It
can run as a normal CLI, or as a Codex `PreToolUse` hook that accelerates narrow,
read-only PowerShell file operations while failing open to the original shell
when anything is uncertain.

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
powershell -NoProfile -ExecutionPolicy Bypass -Command "& ([scriptblock]::Create((iwr https://raw.githubusercontent.com/baitouwuya/slowcatch/master/scripts/install-codex-hook.ps1 -UseB).Content)) -Version v0.1.2"
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
and replaces them during migration. It does not silently edit `config.toml`. If
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
- structured CLI output for downstream tools
- safe Codex hook integration with shell fallback

The hook only replaces commands that match a strict whitelist. Mutating
commands, ambiguous syntax, dynamic PowerShell, external programs, and
unsupported shapes fall back to the original shell.

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
original command.

On uncertainty or backend failure, the hook writes no stdout and exits `0`, so
Codex runs the original shell command unchanged.

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
- large config files: top-level key or section summary
- binary, directory, missing, unreadable, or non-UTF-8 files: fail open to the
  original shell

Line-numbered output keeps original file order and uses edit-friendly line
anchors such as:

```text
L123 | content
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
