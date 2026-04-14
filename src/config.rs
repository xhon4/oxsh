use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub shell: ShellConfig,
    pub prompt: PromptConfig,
    pub history: HistoryConfig,
    pub aliases: HashMap<String, String>,
    pub env: HashMap<String, String>,
    pub path: PathConfig,
    pub on_startup: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct ShellConfig {
    pub auto_cd: bool,
    pub glob: bool,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct PromptConfig {
    pub left: String,
    pub right: String,
    pub vi_mode: bool,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct HistoryConfig {
    pub max_size: usize,
    pub file: String,
    pub ignore_dups: bool,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct PathConfig {
    pub prepend: Vec<String>,
    pub scan_dirs: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shell: ShellConfig::default(),
            prompt: PromptConfig::default(),
            history: HistoryConfig::default(),
            aliases: HashMap::new(),
            env: HashMap::new(),
            path: PathConfig::default(),
            on_startup: Vec::new(),
        }
    }
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            auto_cd: true,
            glob: true,
        }
    }
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self {
            left: "{status}{cwd}{git}{context}".into(),
            right: "{duration}".into(),
            vi_mode: false,
        }
    }
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            max_size: 50000,
            file: "~/.oxsh_history".into(),
            ignore_dups: true,
        }
    }
}

impl Default for PathConfig {
    fn default() -> Self {
        Self {
            prepend: Vec::new(),
            scan_dirs: Vec::new(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let config_path = config_file_path();
        if config_path.exists() {
            return Self::load_from(&config_path);
        }
        Config::default()
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str::<Config>(&content) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("oxsh: config parse error: {e}");
                    Config::default()
                }
            },
            Err(e) => {
                eprintln!("oxsh: {}: {e}", path.display());
                Config::default()
            }
        }
    }

    /// Apply env vars and PATH from config
    pub fn apply_env(&self) {
        // PATH prepend
        let mut path = std::env::var("PATH").unwrap_or_default();
        for dir in self.path.prepend.iter().rev() {
            let expanded = shellexpand::tilde(dir).to_string();
            if std::path::Path::new(&expanded).is_dir() {
                path = format!("{expanded}:{path}");
            }
        }
        // Scan dirs: add all subdirectories
        for scan in &self.path.scan_dirs {
            let expanded = shellexpand::tilde(scan).to_string();
            if let Ok(entries) = std::fs::read_dir(&expanded) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        path = format!("{}:{path}", p.display());
                    }
                }
            }
        }
        unsafe { std::env::set_var("PATH", &path); }

        // Set env vars
        for (key, val) in &self.env {
            let expanded = shellexpand::tilde(val).to_string();
            unsafe { std::env::set_var(key, &expanded); }
        }
    }
}

pub fn config_path() -> PathBuf {
    config_file_path()
}

/// Embedded default config — compiled into the binary.
/// Platform-specific: different defaults for Linux vs Windows.
fn default_config_content() -> &'static str {
    if cfg!(windows) {
        include_str!("defaults/config_windows.toml")
    } else {
        include_str!("defaults/config_linux.toml")
    }
}

/// Generate the default config file. Returns the path written.
pub fn generate_default_config(force: bool) -> Option<PathBuf> {
    let path = default_config_destination();

    if path.exists() && !force {
        println!("Config already exists: {}", path.display());
        println!("Use --init-force to overwrite.");
        return None;
    }

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("oxsh: cannot create {}: {e}", parent.display());
                return None;
            }
        }
    }

    let content = default_config_content();
    match std::fs::write(&path, content) {
        Ok(()) => {
            println!("Config written to: {}", path.display());
            println!("Edit it to customize your shell.");
            Some(path)
        }
        Err(e) => {
            eprintln!("oxsh: cannot write {}: {e}", path.display());
            None
        }
    }
}

/// Where the config file should live by default (for new installs)
fn default_config_destination() -> PathBuf {
    if cfg!(windows) {
        // Windows: %APPDATA%\oxsh\config.toml
        dirs_next::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("oxsh")
            .join("config.toml")
    } else {
        // Linux/macOS: ~/.oxshrc
        dirs_next::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".oxshrc")
    }
}

fn config_file_path() -> PathBuf {
    let home = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("/"));

    // Primary: ~/.oxshrc (Linux/macOS)
    let oxshrc = home.join(".oxshrc");
    if oxshrc.exists() {
        return oxshrc;
    }

    // Windows primary: %APPDATA%\oxsh\config.toml
    if cfg!(windows) {
        if let Some(config_dir) = dirs_next::config_dir() {
            let win_config = config_dir.join("oxsh").join("config.toml");
            if win_config.exists() {
                return win_config;
            }
        }
    }

    // Fallback: ~/.config/oxsh/config.toml (legacy)
    if let Some(config_dir) = dirs_next::config_dir() {
        let legacy = config_dir.join("oxsh").join("config.toml");
        if legacy.exists() {
            eprintln!("oxsh: hint: move {} to ~/.oxshrc", legacy.display());
            return legacy;
        }
    }

    // Return the platform-appropriate default (even if doesn't exist yet)
    default_config_destination()
}
