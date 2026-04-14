use std::path::PathBuf;

/// Full first-time setup: generate config + platform-specific registration.
pub fn run_setup() {
    println!("oxsh {} — first-time setup", env!("CARGO_PKG_VERSION"));
    println!();

    // Step 1: Generate config if missing
    let config_path = crate::config::generate_default_config(false);
    println!();

    // Step 2: Platform-specific setup
    if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
        unix_setup();
    } else if cfg!(windows) {
        windows_setup();
    }

    println!("Setup complete!");
    if let Some(p) = config_path {
        println!("Edit {} to customize your shell.", p.display());
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unix_setup() {
    let binary = current_binary_path();

    // Register in /etc/shells if not already there
    if let Ok(shells) = std::fs::read_to_string("/etc/shells") {
        let bin_str = binary.to_string_lossy();
        if shells.lines().any(|l| l.trim() == bin_str.as_ref()) {
            println!("[ok] oxsh already in /etc/shells");
        } else {
            println!("[..] Adding oxsh to /etc/shells (requires sudo)...");
            let status = std::process::Command::new("sudo")
                .args(["tee", "-a", "/etc/shells"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    if let Some(ref mut stdin) = child.stdin {
                        writeln!(stdin, "{}", bin_str)?;
                    }
                    child.wait()
                });
            match status {
                Ok(s) if s.success() => println!("[ok] Added {} to /etc/shells", bin_str),
                _ => eprintln!("[!!] Failed to add to /etc/shells. You can do it manually:\n     echo {} | sudo tee -a /etc/shells", bin_str),
            }
        }
    }

    // Offer to set as default shell
    println!();
    println!("To set oxsh as your default shell, run:");
    println!("  chsh -s {}", binary.display());
}

#[cfg(windows)]
fn windows_setup() {
    let binary = current_binary_path();
    let bin_dir = binary.parent().unwrap_or(&binary);

    println!("[..] Checking PATH...");

    // Check if binary directory is already in PATH
    if let Ok(path) = std::env::var("PATH") {
        let bin_str = bin_dir.to_string_lossy();
        if path.split(';').any(|p| p == bin_str.as_ref()) {
            println!("[ok] {} already in PATH", bin_str);
            return;
        }
    }

    println!("To add oxsh to your PATH permanently, run in PowerShell (as admin):");
    println!(
        "  [Environment]::SetEnvironmentVariable('PATH', $env:PATH + ';{}', 'User')",
        bin_dir.display()
    );
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn unix_setup() {}
#[cfg(not(windows))]
fn windows_setup() {}

/// Resolve the path to the currently running oxsh binary.
fn current_binary_path() -> PathBuf {
    std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("oxsh"))
        .canonicalize()
        .unwrap_or_else(|_| {
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from("oxsh"))
        })
}
