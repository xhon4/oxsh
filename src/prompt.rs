use crate::config::PromptConfig;
use crate::context::ShellContext;
use crossterm::style::{Color, Stylize};
use reedline::Prompt;
use std::borrow::Cow;
use std::env;
use std::sync::OnceLock;

const SEP: &str = "\u{e0b0}"; // 
const RSEP: &str = "\u{e0b2}"; // 

/// Cache hostname — tries multiple sources for portability across Linux/macOS/BSD.
fn cached_hostname() -> &'static str {
    static HOSTNAME: OnceLock<String> = OnceLock::new();
    HOSTNAME.get_or_init(|| {
        // 1. Linux: /etc/hostname
        if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
            let h = h.trim().to_string();
            if !h.is_empty() {
                return h;
            }
        }
        // 2. $HOSTNAME (bash sets this; also works on many systems)
        if let Ok(h) = env::var("HOSTNAME") {
            if !h.is_empty() {
                return h;
            }
        }
        // 3. `hostname` command (macOS, BSD, Alpine, etc.)
        if let Ok(out) = std::process::Command::new("hostname").output() {
            let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !h.is_empty() {
                return h;
            }
        }
        String::new()
    })
}

struct Segment {
    text: String,
    fg: Color,
    bg: Color,
}

pub struct OxshPrompt {
    pub last_exit_code: i32,
    pub cmd_duration_ms: u128,
    pub context: ShellContext,
    left_format: String,
    right_format: String,
}

impl OxshPrompt {
    pub fn with_context(
        last_exit_code: i32,
        cmd_duration_ms: u128,
        context: ShellContext,
        prompt_config: &PromptConfig,
    ) -> Self {
        Self {
            last_exit_code,
            cmd_duration_ms,
            context,
            left_format: prompt_config.left.clone(),
            right_format: prompt_config.right.clone(),
        }
    }

    fn evaluate_token(&self, token: &str) -> Option<Segment> {
        match token {
            "status" => {
                if self.last_exit_code != 0 {
                    Some(Segment {
                        text: format!(" \u{2718} {} ", self.last_exit_code),
                        fg: Color::White,
                        bg: Color::Red,
                    })
                } else {
                    None
                }
            }
            "cwd" | "cwd:short" => {
                let cwd = env::current_dir().unwrap_or_default();
                let text = if let Some(home) = dirs_next::home_dir() {
                    if let Ok(rest) = cwd.strip_prefix(&home) {
                        if rest.as_os_str().is_empty() {
                            " ~".into()
                        } else {
                            format!(" ~/{}", rest.display())
                        }
                    } else {
                        format!(" {}", cwd.display())
                    }
                } else {
                    format!(" {}", cwd.display())
                };
                Some(Segment { text: format!("{text} "), fg: Color::White, bg: Color::DarkCyan })
            }
            "cwd:full" => {
                let cwd = env::current_dir().unwrap_or_default();
                Some(Segment {
                    text: format!(" {} ", cwd.display()),
                    fg: Color::White,
                    bg: Color::DarkCyan,
                })
            }
            "git" | "git:branch" => {
                self.context.git_branch.as_ref().map(|branch| Segment {
                    text: format!(" \u{e0a0} {branch} "),
                    fg: Color::White,
                    bg: Color::DarkBlue,
                })
            }
            "context" => {
                let mut parts = Vec::new();
                if let Some(ref pt) = self.context.project_type {
                    parts.push(format!("{} {}", pt.icon(), pt.name()));
                }
                if let Some(ref venv) = self.context.virtualenv {
                    parts.push(format!("({venv})"));
                }
                if let Some(ref k8s) = self.context.k8s_context {
                    parts.push(format!("\u{2388}{k8s}"));
                }
                if self.context.in_ssh {
                    parts.push("ssh".into());
                }
                if parts.is_empty() {
                    None
                } else {
                    Some(Segment {
                        text: format!(" {} ", parts.join(" ")),
                        fg: Color::White,
                        bg: Color::DarkMagenta,
                    })
                }
            }
            "project" => {
                self.context.project_type.as_ref().map(|pt| Segment {
                    text: format!(" {} {} ", pt.icon(), pt.name()),
                    fg: Color::White,
                    bg: Color::DarkMagenta,
                })
            }
            "venv" => {
                self.context.virtualenv.as_ref().map(|v| Segment {
                    text: format!(" ({v}) "),
                    fg: Color::White,
                    bg: Color::DarkMagenta,
                })
            }
            "k8s" => {
                self.context.k8s_context.as_ref().map(|k| Segment {
                    text: format!(" \u{2388}{k} "),
                    fg: Color::White,
                    bg: Color::DarkMagenta,
                })
            }
            "ssh" => {
                if self.context.in_ssh {
                    Some(Segment { text: " ssh ".into(), fg: Color::White, bg: Color::DarkYellow })
                } else {
                    None
                }
            }
            "user" => {
                env::var("USER").ok().filter(|u| !u.is_empty()).map(|user| Segment {
                    text: format!(" {user} "),
                    fg: Color::White,
                    bg: Color::DarkGreen,
                })
            }
            "host" => {
                let host = cached_hostname();
                if host.is_empty() {
                    None
                } else {
                    Some(Segment {
                        text: format!(" {host} "),
                        fg: Color::White,
                        bg: Color::DarkGreen,
                    })
                }
            }
            "duration" => {
                if self.cmd_duration_ms < 100 {
                    None
                } else if self.cmd_duration_ms < 1000 {
                    Some(Segment {
                        text: format!(" {}ms ", self.cmd_duration_ms),
                        fg: Color::White,
                        bg: Color::DarkGrey,
                    })
                } else {
                    Some(Segment {
                        text: format!(" {:.1}s ", self.cmd_duration_ms as f64 / 1000.0),
                        fg: Color::White,
                        bg: Color::DarkGrey,
                    })
                }
            }
            _ => None,
        }
    }
}

fn parse_format_tokens(format: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = format.chars().peekable();
    while let Some(&ch) = chars.peek() {
        if ch == '{' {
            chars.next();
            let token: String = chars.by_ref().take_while(|&c| c != '}').collect();
            if !token.is_empty() {
                tokens.push(token);
            }
        } else {
            chars.next();
        }
    }
    tokens
}

fn render_segments(segments: &[Segment]) -> String {
    let mut prompt = String::new();
    for (i, seg) in segments.iter().enumerate() {
        let next_bg = segments.get(i + 1).map(|s| s.bg).unwrap_or(Color::Reset);
        prompt.push_str(&format!(
            "{}{}",
            seg.text.clone().with(seg.fg).on(seg.bg),
            SEP.with(seg.bg).on(next_bg),
        ));
    }
    prompt
}

impl Prompt for OxshPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        let tokens = parse_format_tokens(&self.left_format);
        let segments: Vec<Segment> = tokens.iter()
            .filter_map(|t| self.evaluate_token(t))
            .collect();
        let mut prompt = render_segments(&segments);
        prompt.push(' ');
        Cow::Owned(prompt)
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        let tokens = parse_format_tokens(&self.right_format);
        let segments: Vec<Segment> = tokens.iter()
            .filter_map(|t| self.evaluate_token(t))
            .collect();
        if segments.is_empty() {
            return Cow::Borrowed("");
        }
        let mut prompt = String::new();
        for seg in &segments {
            prompt.push_str(&format!(
                "{}{}",
                RSEP.with(seg.bg),
                seg.text.clone().with(seg.fg).on(seg.bg),
            ));
        }
        Cow::Owned(prompt)
    }

    fn render_prompt_indicator(&self, _mode: reedline::PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("... ")
    }

    fn render_prompt_history_search_indicator(
        &self,
        _history_search: reedline::PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Borrowed("(search) ")
    }
}
