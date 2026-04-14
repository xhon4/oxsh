# 🐂 oxsh

A next-generation hybrid shell built in Rust. oxsh blends POSIX compatibility with structured data pipelines, real-time editing feedback, and environment-aware intelligence — all in a ~1.5 MB self-contained binary.

## ✨ Features

### 🔀 Structured Data Pipelines
- **First-Class JSON Processing**: Pipe structured data through built-in operators like `where`, `select`, `sort-by`, and `to-table` — no `jq` or `awk` needed.
- **Hybrid Execution Engine**: Seamlessly chains external processes with in-process data stages in the same pipeline.
- **Rich Operator Set**: Filter with `where`, project with `select`, aggregate with `count`, slice with `first`/`last`, deduplicate with `uniq`, and more.
- **Word-Based Comparison**: Use `gt`, `lt`, `ge`, `le`, `eq`, `ne` to avoid conflicts with shell redirect syntax.

```bash
# Query an API, shape the data, render a table
curl -s https://api.github.com/repos/rust-lang/rust/releases \
  | from-json | first 5 | select name published_at | to-table

# Filter Kubernetes pods by status
kubectl get pods -o json | from-json | get items \
  | where .status.phase == Running | count
```

### ⚡ Zero-Latency Editing Experience
- **Live Syntax Highlighting**: Commands turn green or red instantly as you type — valid or not found. Flags, strings, numbers, pipes, and variables are all colored distinctly.
- **Background PATH Scanning**: The command cache is built at startup in the background, eliminating per-keystroke filesystem I/O.
- **Skim-Powered Fuzzy Completion**: Intelligent fuzzy matching for commands, files, and directories with context-aware subcommand hints for `git`, `docker`, `kubectl`, `cargo`, `systemctl`, and `pacman`.
- **Fish-Style Autosuggestions**: Grayed-out inline suggestions drawn from your command history.

### 🧭 Environment-Aware Intelligence
- **Automatic Project Detection**: Recognizes Rust, Node.js, Python, Go, Kubernetes, and Docker projects and surfaces relevant context in your prompt.
- **Adaptive Powerline Prompt**: Modular segments for CWD, git branch, project type, command duration, virtualenv, K8s namespace, SSH host, and more — all configurable via TOML.
- **Git Integration**: Branch name and status reflected directly in the prompt without external scripts.

```
 ~/project  main  rust 🦀 ❯
```

### 📂 Navigation & Productivity
- **Browser-Style Directory History**: `back` and `next` commands to traverse your directory history like a browser.
- **Auto-cd**: Type a directory name and you're there — no `cd` required.
- **Last Command Recall**: `!!` expands to your previous command anywhere in the line (e.g., `sudo !!`).
- **Inline Help**: `?? command` fetches a quick explanation without leaving the shell.
- **Universal Glob Expansion**: Native support for `*`, `?`, and `[]` patterns across all commands.

### 🔧 POSIX Compatibility & Scripting
oxsh doesn't force you into a new paradigm. Standard shell constructs work as expected:

```bash
# Loops, conditionals, chains
for f in *.log; do grep -c ERROR "$f"; done
if cargo build; then cargo test; else echo "build failed"; fi
NAME="world"; echo "hello $NAME"
```

Pipes, redirects (`>`, `>>`, `2>`), environment variables, background jobs (`&`), and brace expansion all work out of the box.

**Cross-platform**: Linux, macOS, and Windows.

### 🛠️ Built-in Commands

| Command | Description |
|---------|-------------|
| `cd [dir]` | Change directory (`cd -` for previous) |
| `back` / `next` | Browser-style directory history |
| `alias` / `unalias` | Manage shell aliases |
| `export VAR=val` | Set environment variables |
| `source [file]` | Reload configuration (`reload` alias) |
| `history` | Browse command history |
| `which` / `type` | Locate and identify commands |
| `context` | Show detected project environment |
| `echo`, `pwd`, `true`, `false` | Standard POSIX builtins |

### 🔀 Pipeline Commands Reference

| Command | Description | Example |
|---------|-------------|---------|
| `from-json` | Parse JSON input into structured data | `cat data.json \| from-json` |
| `to-json [--pretty]` | Serialize back to JSON | `... \| to-json -p` |
| `to-table` | Render as a formatted terminal table | `... \| to-table` |
| `where FIELD OP VAL` | Filter records by condition | `... \| where age gt 18` |
| `select F1 F2...` | Project specific fields | `... \| select name email` |
| `sort-by FIELD [--desc]` | Sort by a field | `... \| sort-by score -d` |
| `get FIELD` | Extract nested field values | `... \| get name` |
| `first [N]` / `last [N]` | Take the first or last N items | `... \| first 5` |
| `count` | Count items in a collection | `... \| count` |
| `uniq` | Remove consecutive duplicates | `... \| uniq` |
| `reverse` | Reverse item order | `... \| reverse` |
| `flatten` | Flatten nested arrays | `... \| flatten` |

Comparison operators for `where`: `==`, `!=`, `>`, `<`, `>=`, `<=`, `=~` (contains), `^=` (starts-with).

## 🚀 Quick Start

### Linux / macOS

```bash
git clone https://github.com/xhon4/oxsh.git
cd oxsh
./install.sh
```

This builds the release binary, copies it to `/usr/local/bin`, generates `~/.oxshrc`, and registers oxsh in `/etc/shells`.

### Windows (PowerShell)

```powershell
git clone https://github.com/xhon4/oxsh.git
cd oxsh
.\install.ps1
```

### Manual Build

```bash
cargo build --release
sudo cp target/release/oxsh /usr/local/bin/
oxsh --init   # generate default config
```

## ⚙️ Configuration

All settings live in a single TOML file at `~/.oxshrc` (Linux/macOS) or `%APPDATA%\oxsh\config.toml` (Windows).

```bash
oxsh --init        # generate default config
oxsh --init-force  # overwrite existing config
```

```toml
on_startup = ["fastfetch"]

[shell]
auto_cd = true
glob = true

[prompt]
left = "{status}{cwd}{git}{context}"
right = "{duration}"

[history]
max_size = 50000
file = "~/.oxsh_history"
ignore_dups = true

[aliases]
ll = "eza -l --icons --git"
gs = "git status"
pods = "kubectl get pods -o json"

[env]
EDITOR = "nvim"

[path]
prepend = ["~/.local/bin", "~/.cargo/bin"]
```

Available prompt tokens: `{cwd}`, `{git}`, `{project}`, `{context}`, `{status}`, `{duration}`, `{venv}`, `{k8s}`, `{ssh}`, `{user}`, `{host}`

## 🗺️ Roadmap

- Plugin system (IPC-based, language-agnostic)
- SQLite-backed queryable history
- Async prompt rendering (git status in background)
- Job control (`fg`, `bg`, `jobs`)
- Subshell support `$()`
- Heredocs
- Session recording & replay

## 🤝 Contributing

Contributions are welcome. Feel free to open issues or submit pull requests.

## 📄 License

This project is licensed under the **MIT License**.

## 👤 Author

Developed by **[occhi](https://github.com/xhon4)**.
