use std::env;
use std::path::{Path, PathBuf};

/// Detected project and environment context.
/// The shell uses this for context-aware completion, prompt, and smart defaults.
#[derive(Debug, Clone, Default)]
pub struct ShellContext {
    pub project_type: Option<ProjectType>,
    pub git_repo: bool,
    pub git_branch: Option<String>,
    pub in_ssh: bool,
    pub k8s_context: Option<String>,
    pub virtualenv: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectType {
    Rust,
    Node,
    Python,
    Go,
    Java,
    Kubernetes,
    Docker,
}

impl ProjectType {
    pub fn icon(&self) -> &'static str {
        match self {
            ProjectType::Rust => "\u{e7a8}",     // 
            ProjectType::Node => "\u{e718}",     // 
            ProjectType::Python => "\u{e73c}",   // 
            ProjectType::Go => "\u{e626}",       // 
            ProjectType::Java => "\u{e738}",     // 
            ProjectType::Kubernetes => "\u{fd31}", // ﴱ
            ProjectType::Docker => "\u{f308}",   // 
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ProjectType::Rust => "rust",
            ProjectType::Node => "node",
            ProjectType::Python => "python",
            ProjectType::Go => "go",
            ProjectType::Java => "java",
            ProjectType::Kubernetes => "k8s",
            ProjectType::Docker => "docker",
        }
    }
}

impl std::fmt::Display for ProjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl ShellContext {
    /// Detect context for the current working directory
    pub fn detect() -> Self {
        let cwd = env::current_dir().unwrap_or_default();
        Self {
            project_type: detect_project_type(&cwd),
            git_repo: find_up(&cwd, ".git").is_some(),
            git_branch: detect_git_branch(&cwd),
            in_ssh: env::var("SSH_CONNECTION").is_ok() || env::var("SSH_TTY").is_ok(),
            k8s_context: detect_k8s_context(),
            virtualenv: env::var("VIRTUAL_ENV").ok().map(|v| {
                Path::new(&v)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or(v)
            }),
        }
    }

    /// Get npm scripts from package.json if in a Node project
    pub fn npm_scripts(&self) -> Vec<String> {
        if self.project_type != Some(ProjectType::Node) {
            return Vec::new();
        }
        let cwd = env::current_dir().unwrap_or_default();
        read_npm_scripts(&cwd).unwrap_or_default()
    }

    /// Get cargo subcommands if in a Rust project
    #[allow(dead_code)]
    pub fn cargo_targets(&self) -> Vec<String> {
        if self.project_type != Some(ProjectType::Rust) {
            return Vec::new();
        }
        // Common cargo subcommands
        vec![
            "build", "run", "test", "check", "clippy", "fmt", "doc", "bench",
            "clean", "update", "publish", "install", "add", "remove",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }
}

/// Walk the directory tree once, checking all project markers at each level.
/// Priority within a directory level follows the order below (Rust > Node > Go …).
/// This replaces N separate `find_up` calls (one per project type) with a single walk.
fn detect_project_type(dir: &Path) -> Option<ProjectType> {
    let mut current = dir.to_path_buf();
    loop {
        if current.join("Cargo.toml").exists() {
            return Some(ProjectType::Rust);
        }
        if current.join("package.json").exists() {
            return Some(ProjectType::Node);
        }
        if current.join("go.mod").exists() {
            return Some(ProjectType::Go);
        }
        if current.join("pyproject.toml").exists()
            || current.join("setup.py").exists()
            || current.join("requirements.txt").exists()
        {
            return Some(ProjectType::Python);
        }
        if current.join("pom.xml").exists() || current.join("build.gradle").exists() {
            return Some(ProjectType::Java);
        }
        if current.join("k8s").is_dir()
            || current.join("kubernetes").is_dir()
            || current.join("skaffold.yaml").exists()
        {
            return Some(ProjectType::Kubernetes);
        }
        if current.join("Dockerfile").exists()
            || current.join("docker-compose.yml").exists()
            || current.join("compose.yml").exists()
        {
            return Some(ProjectType::Docker);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Walk up directory tree looking for a file or directory
fn find_up(start: &Path, name: &str) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn detect_git_branch(dir: &Path) -> Option<String> {
    let git_dir = find_up(dir, ".git")?;
    let head = if git_dir.is_file() {
        // Worktree: .git is a file pointing to the real git dir
        let content = std::fs::read_to_string(&git_dir).ok()?;
        let gitdir = content.trim().strip_prefix("gitdir: ")?;
        PathBuf::from(gitdir).join("HEAD")
    } else {
        git_dir.join("HEAD")
    };
    let content = std::fs::read_to_string(head).ok()?;
    let trimmed = content.trim();
    if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
        Some(branch.to_string())
    } else {
        Some(trimmed[..7.min(trimmed.len())].to_string())
    }
}

fn detect_k8s_context() -> Option<String> {
    let kubeconfig = env::var("KUBECONFIG").ok().or_else(|| {
        dirs_next::home_dir().map(|h| h.join(".kube/config").to_string_lossy().to_string())
    })?;
    if !Path::new(&kubeconfig).exists() {
        return None;
    }
    let content = std::fs::read_to_string(&kubeconfig).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(ctx) = trimmed.strip_prefix("current-context:") {
            return Some(ctx.trim().to_string());
        }
    }
    None
}

fn read_npm_scripts(dir: &Path) -> Option<Vec<String>> {
    let pkg = find_up(dir, "package.json")?;
    let content = std::fs::read_to_string(pkg).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let scripts = json.get("scripts")?.as_object()?;
    Some(scripts.keys().cloned().collect())
}
