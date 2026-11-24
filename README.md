# 🔮 oxsh

Hybrid shell built in Rust. oxsh blends POSIX-style shell syntax with structured data pipelines, real-time editing feedback, and environment-aware intelligence.

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
- **Adaptive Powerline Prompt**: Modular segments for CWD, git branch, project type, command duration, virtualenv, Kubernetes context, SSH session indicator, and more — all configurable via TOML.
- **Git Integration**: The current branch is shown directly in the prompt, without external scripts.

```
 ~/project  main  rust 🦀 ❯
```

### 📂 Navigation & Productivity
- **Browser-Style Directory History**: `back` and `next` commands to traverse your directory history like a browser.
- **Auto-cd**: Type a directory name and you're there — no `cd` required.
- **History Expansion**: `!!` expands to your previous command and `!$` to its last argument, anywhere in the line (e.g., `sudo !!`).
- **Inline Help**: `?? command` tells you whether a command is a builtin, a structured pipeline command (with its usage), or an executable on your `PATH` — without running it.
- **Glob Expansion**: Native support for `*`, `?`, and `[]` patterns in command arguments.

### 🔧 Shell Scripting
oxsh supports the everyday shell constructs:

```bash
# Loops, conditionals, variables
for f in app.log db.log; do grep -c ERROR "$f"; done
if cargo build; then cargo test; else echo "build failed"; fi
NAME="world"; echo "hello $NAME"
echo "editor: ${EDITOR:-vi}"
```

Also supported: `for`/`while`/`if` blocks, `&&`/`||` chains, pipes, redirects for external commands (`<`, `>`, `>>`, `2>`, `2>>`, `2>&1`), `VAR=val cmd` prefixes, `${VAR:-default}`, command substitution (`$(...)` and backticks), trailing background jobs (`cmd &`, with `$!`), and brace expansion (`{a,b}`, `{1..5}`).

### ⚠️ Known Limitations
oxsh is not a full POSIX shell yet. Notable gaps, most of them tracked in the [issue tracker](https://github.com/xhon4/oxsh/issues):

- `case`, shell functions, and heredocs are not supported yet.
- A `for`/`while`/`if` block must be the whole command line: it can't be combined with other commands through `;`, `&&` or `||` yet.
- Builtins and control-flow blocks don't take part in pipes and redirections yet ([#11](https://github.com/xhon4/oxsh/issues/11)).
- A failed `&&`/`||` chain also skips independent commands after `;` ([#12](https://github.com/xhon4/oxsh/issues/12)).
- Quotes are not fully preserved before expansion: a quoted `>` can still act as a redirection ([#10](https://github.com/xhon4/oxsh/issues/10), fix pending merge), and brace expansion also applies inside quotes.
- Redirect order is not fully honored, e.g. `cmd > file 2>&1` ([#16](https://github.com/xhon4/oxsh/issues/16)).
- Very large brace ranges are not bounded ([#25](https://github.com/xhon4/oxsh/issues/25)).
- A glob in a `for` list (`for f in *.log`) is passed as a single item.
- Only a trailing `&` starts a background job, and there is no job control yet.

**Platforms**: developed and tested on Linux. The code includes macOS and Windows support, but those platforms are not yet verified in CI ([#27](https://github.com/xhon4/oxsh/issues/27)).

### 🛠️ Built-in Commands

| Command | Description |
|---------|-------------|
| `cd [dir]` | Change directory (`cd -` for previous) |
| `back` / `next` | Browser-style directory history |
| `alias` / `unalias` | Manage shell aliases |
| `export VAR=val` / `unset VAR` | Set or remove environment variables |
| `read [-p prompt] VAR` | Read a line from stdin into a variable |
| `source [file]` | Reload configuration (`reload` alias) |
| `history` | Browse command history |
| `which` / `type` | Locate and identify commands |
| `context` | Show detected project environment |
| `?? command` | Explain what a command is, without running it |
| `help` / `exit` | Show built-in help / exit the shell |
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
| `uniq` | Remove duplicate items (keeps the first occurrence) | `... \| uniq` |
| `reverse` | Reverse item order | `... \| reverse` |
| `flatten` | Flatten nested arrays | `... \| flatten` |

Comparison operators for `where`: `eq`/`==`, `ne`/`!=`, `gt`, `lt`, `ge`, `le`, `=~` (contains), `^=` (starts-with). Use the word forms for ordering comparisons: an unquoted `>` or `<` is parsed as a redirection.

## 🚀 Quick Start

**Requirements**: a stable Rust toolchain with edition 2024 support (install via [rustup](https://rustup.rs)).

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

### Running a single command

```bash
oxsh -c 'ls | wc -l'
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
vi_mode = false

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

Available prompt tokens: `{cwd}`, `{cwd:short}`, `{cwd:full}`, `{git}` (alias `{git:branch}`), `{project}`, `{context}`, `{status}`, `{duration}`, `{venv}`, `{k8s}`, `{ssh}`, `{user}`, `{host}`

## 🗺️ Roadmap

- Plugin system (IPC-based, language-agnostic)
- SQLite-backed queryable history
- Async prompt rendering (git status in background)
- Job control (`fg`, `bg`, `jobs`)
- Heredocs
- Session recording & replay

Planned fixes and smaller improvements are tracked in the [open issues](https://github.com/xhon4/oxsh/issues).

## 🤝 Contributing

Contributions are welcome. Feel free to open issues or submit pull requests.

## 📄 License

This project is licensed under the **MIT License**.

## 👤 Author

Developed by **[occhi](https://github.com/xhon4)**.
