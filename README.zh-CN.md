# slowcatch

捕获慢路径，替换成安全快路径。

`slowcatch` 是一个 Windows 优先的 Rust 本地开发工具。它既可以作为普通 CLI
使用，也可以作为 Codex hook，把一部分安全、只读、形状明确的 PowerShell
文件操作替换成更快的原生后端，并在用户消息中做轻量关键词检索；任何不确定
情况都会 fail-open 回退到原始 shell。

[English README](README.md)

## 一键安装到 Codex

复制下面命令到 PowerShell 运行即可。它会安装最新 GitHub Release，并配置
Codex hook。不需要 clone 或拉取仓库。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "iwr https://raw.githubusercontent.com/baitouwuya/slowcatch/master/scripts/install-codex-hook.ps1 -UseB | iex"
```

重复运行同一命令就是更新。

固定安装某个版本：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "& ([scriptblock]::Create((iwr https://raw.githubusercontent.com/baitouwuya/slowcatch/master/scripts/install-codex-hook.ps1 -UseB).Content)) -Version v0.1.3"
```

安装脚本会从 latest release 下载
`slowcatch-x86_64-pc-windows-msvc.exe`，并保存为：

```text
%USERPROFILE%\.codex\bin\slowcatch.exe
```

然后创建或合并：

```text
%USERPROFILE%\.codex\hooks.json
```

脚本会保留无关 hook，只替换已有的 `slowcatch hook codex` 条目。它也会识别旧
的 `rust-fast-tool hook codex` 条目并在迁移时替换。它会同时安装
`PreToolUse`、`UserPromptSubmit`、`PostToolUse` 和 `Stop` 集成。它不会静默修改 `config.toml`。如果没
有启用 Codex hooks，脚本会打印需要手动添加的片段：

```toml
[features]
codex_hooks = true
```

如果当前 Codex 会话不会自动重载 hook 配置，安装或更新后需要重启 Codex 或
手动重载 hooks。

如果目标 exe 被占用，安装脚本会继续执行，把版本化的
`slowcatch-<version>.exe` 放到旁边，并把 hook 指向这个路径。

Codex 会按会话加载 hook 配置。如果已有 Codex 窗口正在运行，更新后需要重启
Codex 或重载 hooks，避免当前会话继续使用旧的 hook 命令。

### 修复 Hooks JSON

如果 Codex 报错：

```text
failed to parse hooks config ... hooks.json: expected value at line 1 column 1
```

直接在 PowerShell 里重新运行安装命令。安装脚本会用无 BOM UTF-8 重写
`hooks.json`。如果现有文件不是合法 JSON，会先备份为
`hooks.json.bak.<timestamp>`，再创建新的 hook 配置。

## 功能概览

`slowcatch` 主要加速常见的只读慢路径：

- Windows 上通过 Everything 做文件名搜索
- 通过 Rust grep 库做内容搜索
- 流式读取文件行片段
- 优化 `Get-Content` 文件检查
- 为 `Get-ChildItem | Select-Object` 提供投影输出
- 通过 Everything 路径/文件名搜索做 prompt UUID lookup
- 为 CLI 提供结构化输出，方便后续管道处理
- 为 Codex hook 提供带 shell 兜底的安全集成

Codex hook 只替换白名单命中的命令。变更型命令、歧义语法、动态 PowerShell、
外部程序和不支持的形状都会回退到原始 shell 执行。

当用户消息里出现 UUID-like 编码时，`slowcatch` 可以按路径或文件名查找相关
文件，并在继续处理前把简短的 `additionalContext` 注入给 Codex。

## CLI 用法

搜索文件名：

```powershell
slowcatch find Cargo.toml --root E:\GitHub --limit 20
```

搜索文件内容：

```powershell
slowcatch grep "FAST_PATH_SUCCESS" E:\GitHub\slowcatch --limit 50
```

读取文件的一段行窗口，避免 PowerShell 把整文件变成对象流：

```powershell
slowcatch slice E:\GitHub\slowcatch\src\shell_parse\mod.rs --skip 200 --first 80
```

检查本地解析器和后端可用性：

```powershell
slowcatch self-test
```

## 结构化输出

`find`、`grep`、`slice` 支持：

```text
--output text|json|jsonl
--select column1,column2
--where column=value
--where column~substring
--sort-by column
--limit N
```

示例：

```powershell
slowcatch find *.rs --root E:\GitHub\slowcatch --output jsonl --select path,extension --where extension=.rs
slowcatch grep "ParseDecision" E:\GitHub\slowcatch --output json --select path,line_number,line --sort-by path --limit 20
slowcatch slice README.md --skip 0 --first 30 --output jsonl
```

可用字段：

- `find`: `path`, `name`, `extension`, `parent`
- `grep`: `path`, `name`, `extension`, `parent`, `line_number`, `line`
- `slice`: `line_number`, `text`

## Codex Hook Fast Paths

安装模式会配置：

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

当前支持的 PowerShell 形状包括：

- `Get-ChildItem/gci/dir/ls ... -Recurse ... -Filter/-Include ...`
- `Get-ChildItem ... | Select-Object -ExpandProperty FullName`
- `Get-ChildItem ... | Select-Object FullName,Length,LastWriteTime,Mode,Name`
- `Select-String -Path ... -Pattern ...`
- `Select-String -Path ... -Pattern ... -Context A,B`
- `Get-ChildItem ... -Recurse | Select-String ...`
- `Get-Content file | Select-String ... -Context A,B`
- `Get-Content file | Select-Object -Skip N -First M`
- 简单 `Get-Content`、`gc`、`cat`、`type` 完整文件检查
- 安全的顶层 `;` 命令列表，每一段都必须是已知只读 fast-path，或严格白名单
  内的小型 git 状态命令

命中成功时，hook 返回 Codex block decision，reason 以：

```text
FAST_PATH_SUCCESS
```

开头。这个消息包含替代输出，并明确告诉 Codex 不要重试原命令。Fast-path 输出
会使用紧凑的行式头部：

```text
slowcatch_result v1 text
kind=<operation_kind> status=<ok|empty> items=<n>

<body>
```

命令列表和 mini-script 会使用类似
`segment 1 kind=list_directory status=ok items=3` 的段头。成功但为空的结果使用
`status=empty items=0`，正文为 `no results`。

不确定或后端失败时，hook 不输出 stdout 并以 `0` 退出，Codex 会继续执行原始
shell 命令。

在 `apply_patch` 编辑后，`PostToolUse` hook 默认保持安静。只有当改动文件里
出现冲突标记、误留的 patch 标记、明显占位/调试代码等高置信问题时，才返回
`slowcatch_post_edit_check v1 issues`，让 Codex 立即回头修复。

编辑后快检刻意保持本地、轻量和保守。它只读取本轮改动文件，限制单轮工作量，
并且对缺失、二进制、非 UTF-8、过大、目录、不可读或未知类型文件 fail-open。
当前会报告：

- Git 冲突标记、误留 patch 标记、显式删除占位符等 error，例如
  `TODO&#95;REMOVE` 和 `FIXME&#95;REMOVE`
- 大括号类语言里明确的 delimiter 不平衡 error
- Rust 调试占位 warning：`dbg!`、`todo!()`、`unimplemented!()`
- Rust、Python、C、C++/头文件、GDScript 的轻量 tree-sitter 语法解析 warning

快检在套用规则前会按语言遮罩注释、字符串、Markdown fenced code block 和其他
非代码区域，尽量减少误报。marker 检查覆盖当前支持的代码/配置/文本路径，包括
Rust、Python、JavaScript/TypeScript、JSX/TSX、C/C++ 头文件、Go、Java、C#、
GDScript、JSON、TOML、YAML、CSS/SCSS、HTML/XML、Markdown/MDX、Dockerfile、
Makefile、`.env*`、`.gitignore`、`.gitattributes` 和 `.editorconfig`。它不是编译器
或 LSP 替代；项目级诊断由下面的 Stop-time 检查负责。

Rust 项目级诊断需要显式开启。在 Cargo 项目根目录创建 `.slowcatch.toml`：

```toml
[post_edit.project_check]
enabled = true
kind = "rust"
command = "cargo check -q"
timeout_seconds = 15
trigger = "stop"
changed_files = ["*.rs"]
```

当一轮对话编辑了 Rust 文件，`Stop` hook 会在回合结束时运行一次配置的命令。
检查成功、超时、命令缺失、没有配置或没有相关 `.rs` 改动都会保持安静。真实
诊断会返回 `slowcatch_project_check v1 failed`，让 Codex 带着编译输出继续修。

C++ 项目级诊断也需要显式开启。把 `.slowcatch.toml` 放在已有
`compile_commands.json` 旁边，或指向它所在目录：

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

当一轮对话编辑了 C/C++ 文件，`Stop` hook 会对变更文件运行
`clangd --check`，并限制单轮检查数量。Slowcatch 要求已有
`compile_commands.json`，不会运行 CMake 或生成构建文件。`clangd` 缺失、编译库
缺失、超时或无关改动都会保持安静。

对于 prompt lookup，用户消息中的 UUID-like token 可能触发一次很轻量的
Everything 搜索。hook 只注入路径名，不读取文件内容，并且会刻意保持输出短小：

```text
slowcatch_refs v1 paths_only
uuid:019e0b7c-4f63-7bc1-8d24-0586a9098481 [1/1]
C:\Users\you\.codex\sessions\rollout-019e0b7c-4f63-7bc1-8d24-0586a9098481.jsonl
```

排查 prompt lookup 行为时，可以设置 `SLOWCATCH_PROMPT_DEBUG=1`，让注入上下文
额外包含 detector 诊断信息。

Hook fast-path 还有内部运行预算。慢后端会在 `slowcatch` 内部超时，记录为后端
失败并 fail-open，避免拖到 Codex 自己的 hook timeout。

## 文件检查策略

对于简单 `Get-Content` fast-path，hook 会按文件类型和大小优化输出：

- 小代码文件：完整输出，并加原始行号
- 小配置和 Markdown 文件：更高阈值的完整行号输出
- 大代码文件：使用 tree-sitter 输出大纲，首批支持 Rust、Python、C、C++、
  头文件和 GDScript
- 大 Markdown 文件：输出 heading 树和 fenced code block 行号
- 大配置和 lock 文件：输出顶层 key 或 section 摘要
- 二进制、目录、缺失、不可读、非 UTF-8 文件：fail-open 回退到原 shell

文件检查输出会包含紧凑元信息：

```text
kind=inspect_file status=ok
file_path=<path>
file_kind=<code|markdown|config|lockfile|text|unknown>
render=<full|outline|summary|preview>
bytes=<n> lines=<n> line_numbers=original
line_format=source:L<n>| summary:L<n> <type>|
```

完整原文输出不重排行，使用省 token 且适合编辑定位的格式：

```text
L123 | content
```

生成的摘要使用带类型的记录：

```text
L18 symbol | function parse_powershell range=L18-L30
L42 heading | h2 File Inspection
L50 fence | range=L50-L54 lang=powershell
L7 key | dependencies
```

## 安全模型

hook 分类器有三种结果：

- `Fast`：安全白名单命中，执行 Rust 后端并替代输出
- `PassThrough`：明确不是候选、变更型命令、风险语法或外部程序，直接放行
- `UnknownCandidate`：看起来是文件/内容处理链路但暂不支持，记录后放行

未知候选和 fast-path 后端失败会追加 JSONL 到：

```text
%USERPROFILE%\.codex\logs\slowcatch-unknown.jsonl
```

日志写入失败会被忽略，永远不会阻止原始 shell 兜底。

外部程序不会被广泛拦截。只有在安全命令列表中，才允许直接执行这些只读 git
形式：

- `git status --short`
- `git status --porcelain`
- `git status --porcelain=v1`
- `git status --porcelain=v2`
- `git diff --stat`
- `git log --oneline -n N`，其中 `N` 必须在 `1..=50`

## 从源码构建

```powershell
git clone https://github.com/baitouwuya/slowcatch.git
cd slowcatch
cargo test
cargo build --release
```

可选 Everything 现场测试：

```powershell
cargo test --features live-everything
```

## License

GPL-3.0-or-later。见 [LICENSE](LICENSE)。
