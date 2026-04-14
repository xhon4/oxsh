use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use reedline::{Completer, Span, Suggestion};
use crate::context::ShellContext;

pub struct OxshCompleter {
    commands: Vec<String>,
    matcher: SkimMatcherV2,
}

impl OxshCompleter {
    pub fn new(commands: Vec<String>) -> Self {
        Self {
            commands,
            matcher: SkimMatcherV2::default(),
        }
    }
}

// ── Subcommand tables: (name, short description) ──

static GIT_SUBS: &[(&str, &str)] = &[
    ("add", "Stage changes"),
    ("bisect", "Binary search for bugs"),
    ("branch", "List/create/delete branches"),
    ("checkout", "Switch branches or restore files"),
    ("cherry-pick", "Apply commit from another branch"),
    ("clone", "Clone a repository"),
    ("commit", "Record changes"),
    ("diff", "Show changes"),
    ("fetch", "Download objects and refs"),
    ("init", "Create empty repo"),
    ("log", "Show commit history"),
    ("merge", "Join branches"),
    ("pull", "Fetch and merge"),
    ("push", "Update remote"),
    ("rebase", "Reapply commits"),
    ("remote", "Manage remotes"),
    ("reset", "Reset HEAD"),
    ("restore", "Restore working tree files"),
    ("revert", "Revert a commit"),
    ("rm", "Remove files from tracking"),
    ("show", "Show object"),
    ("stash", "Stash changes"),
    ("status", "Show working tree status"),
    ("switch", "Switch branches"),
    ("tag", "Create/list/delete tags"),
    ("worktree", "Manage worktrees"),
];

static DOCKER_SUBS: &[(&str, &str)] = &[
    ("build", "Build an image"),
    ("compose", "Docker Compose"),
    ("exec", "Run in container"),
    ("images", "List images"),
    ("inspect", "Inspect object"),
    ("logs", "View container logs"),
    ("network", "Manage networks"),
    ("ps", "List containers"),
    ("pull", "Pull an image"),
    ("push", "Push an image"),
    ("rm", "Remove containers"),
    ("rmi", "Remove images"),
    ("run", "Create and run container"),
    ("stop", "Stop containers"),
    ("volume", "Manage volumes"),
];

static KUBECTL_SUBS: &[(&str, &str)] = &[
    ("apply", "Apply config to resource"),
    ("config", "Modify kubeconfig"),
    ("create", "Create resource"),
    ("delete", "Delete resources"),
    ("describe", "Show resource details"),
    ("edit", "Edit resource"),
    ("exec", "Execute in container"),
    ("expose", "Expose as service"),
    ("get", "Display resources"),
    ("label", "Update labels"),
    ("logs", "Print container logs"),
    ("patch", "Update fields"),
    ("port-forward", "Forward ports"),
    ("rollout", "Manage rollouts"),
    ("run", "Run image"),
    ("scale", "Scale deployment"),
    ("set", "Set resource fields"),
    ("top", "Resource usage"),
];

static SYSTEMCTL_SUBS: &[(&str, &str)] = &[
    ("start", "Start unit"),
    ("stop", "Stop unit"),
    ("restart", "Restart unit"),
    ("reload", "Reload unit config"),
    ("enable", "Enable at boot"),
    ("disable", "Disable at boot"),
    ("status", "Show unit status"),
    ("is-active", "Check if active"),
    ("is-enabled", "Check if enabled"),
    ("list-units", "List loaded units"),
    ("daemon-reload", "Reload systemd"),
];

static CARGO_SUBS: &[(&str, &str)] = &[
    ("build", "Compile project"),
    ("run", "Run binary"),
    ("test", "Run tests"),
    ("check", "Check without building"),
    ("clippy", "Run lints"),
    ("fmt", "Format code"),
    ("clean", "Remove target/"),
    ("doc", "Build docs"),
    ("update", "Update deps"),
    ("add", "Add dependency"),
    ("remove", "Remove dependency"),
    ("init", "Create new project in dir"),
    ("new", "Create new project"),
    ("bench", "Run benchmarks"),
    ("publish", "Publish to crates.io"),
    ("install", "Install binary"),
    ("tree", "Show dependency tree"),
];

static PACMAN_SUBS: &[(&str, &str)] = &[
    ("-S", "Install package"),
    ("-Ss", "Search packages"),
    ("-Syu", "Full system upgrade"),
    ("-Syy", "Force refresh db"),
    ("-R", "Remove package"),
    ("-Rs", "Remove with deps"),
    ("-Rns", "Remove + config + deps"),
    ("-Q", "Query installed"),
    ("-Qs", "Search installed"),
    ("-Qi", "Package info"),
    ("-Qe", "Explicitly installed"),
    ("-Ql", "List package files"),
    ("-Sc", "Clean cache"),
];

// ── Completer implementation ──

impl Completer for OxshCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let line_to_pos = &line[..pos];
        let word_start = line_to_pos
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);
        let partial = &line_to_pos[word_start..];
        let before = line_to_pos[..word_start].trim();

        let is_cmd_position = before.is_empty()
            || before.ends_with("&&")
            || before.ends_with("||")
            || before.ends_with(';')
            || before.ends_with('|');

        let span = Span::new(word_start, pos);

        if is_cmd_position {
            // If it looks like a path, complete paths even in command position
            if partial.starts_with('/')
                || partial.starts_with('.')
                || partial.starts_with('~')
            {
                return complete_paths(partial, span, false);
            }
            self.complete_commands(partial, span)
        } else {
            let words: Vec<&str> = before.split_whitespace().collect();
            let first_word = words.first().copied().unwrap_or("");

            // cd / pushd: directories only
            if matches!(first_word, "cd" | "pushd" | "mkdir") && words.len() == 1 {
                return complete_paths(partial, span, true);
            }

            // Subcommand completion for known tools (only after the base command)
            if words.len() == 1 {
                if let Some(suggestions) =
                    complete_subcommands(first_word, partial, span, &self.matcher)
                {
                    if !suggestions.is_empty() {
                        return suggestions;
                    }
                }
            }

            // Dynamic context-aware: `npm run <script>`, `cargo test <target>`
            if words.len() == 2 {
                let ctx = ShellContext::detect();
                if let Some(suggestions) =
                    complete_dynamic_subcommands(first_word, words[1], partial, span, &self.matcher, &ctx)
                {
                    if !suggestions.is_empty() {
                        return suggestions;
                    }
                }
            }

            complete_paths(partial, span, false)
        }
    }
}

impl OxshCompleter {
    fn complete_commands(&self, partial: &str, span: Span) -> Vec<Suggestion> {
        if partial.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<(&String, i64)> = self
            .commands
            .iter()
            .filter_map(|c| {
                self.matcher
                    .fuzzy_match(c, partial)
                    .map(|score| (c, score))
            })
            .collect();

        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

        scored
            .into_iter()
            .take(40)
            .map(|(c, _)| Suggestion {
                value: c.clone(),
                description: None,
                style: None,
                extra: None,
                span,
                append_whitespace: true,
                display_override: None,
                match_indices: None,
            })
            .collect()
    }
}

fn complete_subcommands(
    cmd: &str,
    partial: &str,
    span: Span,
    matcher: &SkimMatcherV2,
) -> Option<Vec<Suggestion>> {
    let table: &[(&str, &str)] = match cmd {
        "git" => GIT_SUBS,
        "docker" | "podman" => DOCKER_SUBS,
        "kubectl" | "k" => KUBECTL_SUBS,
        "systemctl" => SYSTEMCTL_SUBS,
        "cargo" => CARGO_SUBS,
        "pacman" | "paru" | "yay" => PACMAN_SUBS,
        _ => return None,
    };

    let mut scored: Vec<(&str, &str, i64)> = table
        .iter()
        .filter_map(|(name, desc)| {
            if partial.is_empty() {
                Some((*name, *desc, 0))
            } else {
                matcher
                    .fuzzy_match(name, partial)
                    .map(|score| (*name, *desc, score))
            }
        })
        .collect();

    scored.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(b.0)));

    Some(
        scored
            .into_iter()
            .take(30)
            .map(|(name, desc, _)| Suggestion {
                value: name.to_string(),
                description: Some(desc.to_string()),
                style: None,
                extra: None,
                span,
                append_whitespace: true,
                display_override: None,
                match_indices: None,
            })
            .collect(),
    )
}

/// Dynamic completions based on project context (npm scripts, cargo targets, etc.)
fn complete_dynamic_subcommands(
    cmd: &str,
    subcmd: &str,
    partial: &str,
    span: Span,
    matcher: &SkimMatcherV2,
    ctx: &ShellContext,
) -> Option<Vec<Suggestion>> {
    let items: Vec<(String, String)> = match (cmd, subcmd) {
        ("npm" | "yarn" | "pnpm", "run") => {
            ctx.npm_scripts()
                .into_iter()
                .map(|s| (s.clone(), format!("npm script: {s}")))
                .collect()
        }
        ("cargo", "test" | "run" | "bench" | "example") => {
            // Read Cargo.toml for [[bin]], [[example]], [[test]], [[bench]] names
            read_cargo_targets(subcmd)
                .into_iter()
                .map(|s| (s.clone(), format!("cargo {subcmd} target")))
                .collect()
        }
        _ => return None,
    };

    if items.is_empty() {
        return None;
    }

    let mut scored: Vec<(String, String, i64)> = items
        .into_iter()
        .filter_map(|(name, desc)| {
            if partial.is_empty() {
                Some((name, desc, 0))
            } else {
                matcher
                    .fuzzy_match(&name, partial)
                    .map(|score| (name, desc, score))
            }
        })
        .collect();

    scored.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));

    Some(
        scored
            .into_iter()
            .take(30)
            .map(|(name, desc, _)| Suggestion {
                value: name,
                description: Some(desc),
                style: None,
                extra: None,
                span,
                append_whitespace: true,
                display_override: None,
                match_indices: None,
            })
            .collect(),
    )
}

/// Read Cargo.toml for target names relevant to the given subcommand
fn read_cargo_targets(subcmd: &str) -> Vec<String> {
    let content = match std::fs::read_to_string("Cargo.toml") {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let table: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let section = match subcmd {
        "test" => "test",
        "bench" => "bench",
        "example" => "example",
        "run" => "bin",
        _ => return Vec::new(),
    };

    // Check [[bin]], [[test]], [[example]], [[bench]] arrays
    let mut names = Vec::new();
    if let Some(arr) = table.get(section).and_then(|v| v.as_array()) {
        for entry in arr {
            if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                names.push(name.to_string());
            }
        }
    }
    // Also add the package name for `cargo run`
    if subcmd == "run" {
        if let Some(pkg) = table.get("package").and_then(|v| v.get("name")).and_then(|v| v.as_str()) {
            if !names.contains(&pkg.to_string()) {
                names.push(pkg.to_string());
            }
        }
    }
    names
}

fn complete_paths(partial: &str, span: Span, dirs_only: bool) -> Vec<Suggestion> {
    let expanded = shellexpand::tilde(partial).to_string();

    let (dir, prefix) = match expanded.rfind('/') {
        Some(i) => (
            if i == 0 {
                "/".into()
            } else {
                expanded[..i].to_string()
            },
            expanded[i + 1..].to_string(),
        ),
        None => (".".into(), expanded),
    };

    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(String, bool, i64)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let n = name.to_string_lossy().to_string();

            // Hide dotfiles unless prefix starts with .
            if n.starts_with('.') && !prefix.starts_with('.') {
                return None;
            }

            let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if dirs_only && !is_dir {
                return None;
            }

            // Score: exact prefix gets a big boost, fuzzy gets base score
            let score = if prefix.is_empty() {
                // Show all when no prefix typed yet
                0
            } else if n.starts_with(&*prefix) {
                // Prefix match: high score (feels natural like zsh)
                1000 + (100 - n.len() as i64).max(0)
            } else {
                // Fuzzy: lower score
                matcher.fuzzy_match(&n, &prefix)?
            };

            Some((n, is_dir, score))
        })
        .collect();

    scored.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));

    scored
        .into_iter()
        .take(50)
        .map(|(n, is_dir, _)| {
            let suf = if is_dir { "/" } else { "" };
            let value = if partial.contains('/') {
                format!("{}{n}{suf}", &partial[..partial.rfind('/').unwrap() + 1])
            } else {
                format!("{n}{suf}")
            };
            Suggestion {
                value,
                description: None,
                style: None,
                extra: None,
                span,
                append_whitespace: !is_dir,
                display_override: None,
                match_indices: None,
            }
        })
        .collect()
}
