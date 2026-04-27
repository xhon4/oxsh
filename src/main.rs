mod builtins;
mod completer;
mod config;
mod context;
mod executor;
mod highlight;
mod parser;
mod prompt;
mod scripting;
mod setup;
mod shell;
mod structured;
mod value;

use completer::OxshCompleter;
use config::Config;
use highlight::OxshHighlighter;
use reedline::{
    default_emacs_keybindings, default_vi_insert_keybindings, default_vi_normal_keybindings,
    ColumnarMenu, EditMode, Emacs, FileBackedHistory, KeyCode,
    KeyModifiers, MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu,
    DefaultHinter, Vi,
};
use nu_ansi_term::{Color, Style};
use std::collections::HashMap;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.contains(&"--version".into()) || args.contains(&"-v".into()) {
        println!("oxsh {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    if args.contains(&"--help".into()) || args.contains(&"-h".into()) {
        print_usage();
        return;
    }

    // --init: generate default config file
    if args.contains(&"--init".into()) {
        config::generate_default_config(false);
        return;
    }

    // --init --force: overwrite existing config
    if args.contains(&"--init-force".into()) {
        config::generate_default_config(true);
        return;
    }

    // --setup: full first-time setup (config + register shell on Linux)
    if args.contains(&"--setup".into()) {
        setup::run_setup();
        return;
    }

    let config = Config::load();
    config.apply_env();

    // oxsh -c "command [args...]"
    // Route through the full shell engine so `;`, `&&`, loops, and all
    // builtins (including stateful ones like `read`) work correctly.
    if args.len() > 2 && args[1] == "-c" {
        let line_editor = Reedline::create();
        let mut sh = shell::Shell::new(config, line_editor);
        // Expose positional parameters ($1, $2, ...) for script use
        for (i, arg) in args.iter().skip(3).enumerate() {
            sh.set_positional(i + 1, arg);
        }
        let exit_code = sh.run_command(&args[2]);
        std::process::exit(exit_code);
    }

    // ── Startup: parallelize PATH scan with on_startup commands ──
    // Kick off PATH scan in background thread while startup commands run
    let aliases_clone = config.aliases.clone();
    let path_handle = std::thread::spawn(move || collect_path_commands(&aliases_clone));

    // Run on_startup commands (e.g., fastfetch) — while PATH scan runs in parallel
    for cmd in &config.on_startup {
        let pipeline = parser::parse_pipeline(cmd);
        executor::execute_pipeline(pipeline);
    }

    // Setup history
    let history_path = shellexpand::tilde(&config.history.file).to_string();
    let history =
        FileBackedHistory::with_file(config.history.max_size, history_path.into())
            .expect("Failed to create history file");

    // Setup keybindings — support vi_mode from config
    let edit_mode: Box<dyn EditMode> = if config.prompt.vi_mode {
        let mut insert_kb = default_vi_insert_keybindings();
        let normal_kb = default_vi_normal_keybindings();
        insert_kb.add_binding(
            KeyModifiers::CONTROL,
            KeyCode::Char('l'),
            ReedlineEvent::ExecuteHostCommand("clear".into()),
        );
        insert_kb.add_binding(
            KeyModifiers::CONTROL,
            KeyCode::Char('r'),
            ReedlineEvent::SearchHistory,
        );
        insert_kb.add_binding(
            KeyModifiers::NONE,
            KeyCode::Tab,
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu("completion_menu".to_string()),
                ReedlineEvent::MenuNext,
            ]),
        );
        Box::new(Vi::new(insert_kb, normal_kb))
    } else {
        let mut keybindings = default_emacs_keybindings();
        keybindings.add_binding(
            KeyModifiers::CONTROL,
            KeyCode::Char('l'),
            ReedlineEvent::ExecuteHostCommand("clear".into()),
        );
        keybindings.add_binding(
            KeyModifiers::CONTROL,
            KeyCode::Char('r'),
            ReedlineEvent::SearchHistory,
        );
        keybindings.add_binding(
            KeyModifiers::NONE,
            KeyCode::Tab,
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu("completion_menu".to_string()),
                ReedlineEvent::MenuNext,
            ]),
        );
        keybindings.add_binding(
            KeyModifiers::NONE,
            KeyCode::Up,
            ReedlineEvent::UntilFound(vec![ReedlineEvent::MenuUp, ReedlineEvent::Up]),
        );
        keybindings.add_binding(
            KeyModifiers::NONE,
            KeyCode::Down,
            ReedlineEvent::UntilFound(vec![ReedlineEvent::MenuDown, ReedlineEvent::Down]),
        );
        Box::new(Emacs::new(keybindings))
    };

    // Setup completer with PATH commands + builtins (collected in background thread)
    let commands = path_handle.join().unwrap_or_default();
    let highlighter = OxshHighlighter::new();
    highlighter.seed_commands(&commands);
    let completer = Box::new(OxshCompleter::new(commands.clone()));
    let completion_menu = Box::new(
        ColumnarMenu::default()
            .with_name("completion_menu")
            .with_columns(4)
            .with_column_padding(2),
    );

    // Fish-style autosuggestions from history
    let hinter = Box::new(
        DefaultHinter::default().with_style(Style::new().fg(Color::DarkGray)),
    );

    // Build reedline
    let line_editor = Reedline::create()
        .with_history(Box::new(history))
        .with_history_session_id(Reedline::create_history_session_id())
        .with_history_exclusion_prefix(Some(" ".to_string()))
        .with_edit_mode(edit_mode)
        .with_highlighter(Box::new(highlighter))
        .with_completer(completer)
        .with_hinter(hinter)
        .with_quick_completions(true)
        .with_partial_completions(true)
        .with_menu(ReedlineMenu::EngineCompleter(completion_menu));

    let mut shell = shell::Shell::new(config, line_editor);
    // Provide pre-scanned command list so typo suggestions don't re-scan PATH
    shell.seed_known_commands(commands);
    shell.run();
}

/// Collect all executable names from PATH for completion.
/// Optimized: skips per-file lstat() — everything in PATH dirs is assumed executable.
fn collect_path_commands(aliases: &HashMap<String, String>) -> Vec<String> {
    let mut commands: Vec<String> = Vec::with_capacity(4096);

    // Builtins + structured commands (instant)
    for &b in builtins::BUILTIN_NAMES {
        commands.push(b.to_string());
    }
    for name in structured::STRUCTURED_COMMANDS {
        commands.push(name.to_string());
    }
    for name in aliases.keys() {
        commands.push(name.clone());
    }

    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if dir.is_empty() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                // Just take the name — skip file_type() which triggers lstat() per file.
                // Directories in PATH are rare and harmless as completion candidates.
                if let Some(name) = entry.file_name().to_str() {
                    if !name.starts_with('.') {
                        commands.push(name.to_string());
                    }
                }
            }
        }
    }

    commands.sort_unstable();
    commands.dedup();
    commands
}

fn print_usage() {
    println!("oxsh {} — next-gen hybrid shell", env!("CARGO_PKG_VERSION"));
    println!();
    println!("USAGE:");
    println!("  oxsh                    Start interactive shell");
    println!("  oxsh -c \"command\"       Execute a command and exit");
    println!("  oxsh --init             Generate default config (~/.oxshrc)");
    println!("  oxsh --init-force       Overwrite existing config");
    println!("  oxsh --setup            Full first-time setup");
    println!("  oxsh --version          Show version");
    println!("  oxsh --help             Show this help");
    println!();
    println!("CONFIG:");
    println!("  Linux/macOS:  ~/.oxshrc");
    println!("  Windows:      %APPDATA%\\oxsh\\config.toml");
    println!();
    println!("Run 'help' inside the shell for builtin commands.");
}
