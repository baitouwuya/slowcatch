# slowcatch

捕获慢路径，替换成安全快路径。

`slowcatch` 是一个 Windows 优先的 Rust 本地开发工具。它既可以作为普通 CLI
使用，也可以作为 Codex `PreToolUse` hook，把一部分安全、只读、形状明确的
PowerShell 文件操作替换成更快的原生后端；任何不确定情况都会 fail-open
回退到原始 shell。

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
powershell -NoProfile -ExecutionPolicy Bypass -Command "& ([scriptblock]::Create((iwr https://raw.githubusercontent.com/baitouwuya/slowcatch/master/scripts/install-codex-hook.ps1 -UseB).Content)) -Version v0.1.1"
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
的 `rust-fast-tool hook codex` 条目并在迁移时替换。它不会静默修改
`config.toml`。如果没有启用 Codex hooks，脚本会打印需要手动添加的片段：

```toml
[features]
codex_hooks = true
```

如果当前 Codex 会话不会自动重载 hook 配置，安装或更新后需要重启 Codex 或
手动重载 hooks。

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
- 为 CLI 提供结构化输出，方便后续管道处理
- 为 Codex hook 提供带 shell 兜底的安全集成

Codex hook 只替换白名单命中的命令。变更型命令、歧义语法、动态 PowerShell、
外部程序和不支持的形状都会回退到原始 shell 执行。

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
            "timeout": 10,
            "statusMessage": "Checking slowcatch fast-path"
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

开头。这个消息包含替代输出，并明确告诉 Codex 不要重试原命令。

不确定或后端失败时，hook 不输出 stdout 并以 `0` 退出，Codex 会继续执行原始
shell 命令。

Hook fast-path 还有内部运行预算。慢后端会在 `slowcatch` 内部超时，记录为后端
失败并 fail-open，避免拖到 Codex 自己的 hook timeout。

## 文件检查策略

对于简单 `Get-Content` fast-path，hook 会按文件类型和大小优化输出：

- 小代码文件：完整输出，并加原始行号
- 小配置和 Markdown 文件：更高阈值的完整行号输出
- 大代码文件：使用 tree-sitter 输出大纲，首批支持 Rust、Python、C、C++、
  头文件和 GDScript
- 大 Markdown 文件：输出 heading 树和 fenced code block 行号
- 大配置文件：输出顶层 key 或 section 摘要
- 二进制、目录、缺失、不可读、非 UTF-8 文件：fail-open 回退到原 shell

行号输出不重排原文，使用适合编辑定位的格式：

```text
L123 | content
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
