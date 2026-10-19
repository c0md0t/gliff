//! Remembering the last machine, and installing for the current user.

use std::path::PathBuf;

const LAST_HOST: &str = "win-last-host";

/// Where gliff-win keeps its settings: `%APPDATA%\gliff` on Windows, the
/// XDG config directory elsewhere.
fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?
    };
    Some(base.join("gliff"))
}

/// The machine connected to last time.
pub fn last_host() -> Option<String> {
    let text = std::fs::read_to_string(config_dir()?.join(LAST_HOST)).ok()?;
    let host = text.trim();
    (!host.is_empty()).then(|| host.to_string())
}

pub fn remember_host(host: &str) {
    let Some(dir) = config_dir() else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(dir.join(LAST_HOST), format!("{host}\n")))
    {
        tracing::warn!(error = %e, "cannot remember the host");
    }
}

/// Ask on the console which machine to connect to.
pub fn ask_host() -> Option<String> {
    use std::io::Write;
    eprint!("gliff: connect to (user@host): ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    let host = line.trim();
    (!host.is_empty()).then(|| host.to_string())
}

/// Copy this program to `%LOCALAPPDATA%\Programs\gliff`, put that folder
/// on the user's PATH, and add a "gliff" Start menu shortcut that opens
/// full screen to the last machine.
#[cfg(windows)]
pub fn install() -> anyhow::Result<()> {
    use anyhow::Context;

    let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    let appdata = std::env::var_os("APPDATA").context("APPDATA is not set")?;
    let dir = PathBuf::from(local).join("Programs").join("gliff");
    let target = dir.join("gliff-win.exe");
    let shortcut = PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\gliff.lnk");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let exe = std::env::current_exe()?;
    // The installed copy reinstalling itself only refreshes the shortcut.
    if exe.canonicalize().ok() != target.canonicalize().ok() {
        std::fs::copy(&exe, &target).with_context(|| {
            format!(
                "copy to {} (is gliff running from there?)",
                target.display()
            )
        })?;
    }
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:GLIFF_LNK)
$s.TargetPath = $env:GLIFF_EXE
$s.Arguments = '--fullscreen'
$s.WorkingDirectory = $env:GLIFF_DIR
$s.Description = 'gliff remote desktop'
$s.Save()
$path = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($path -split ';') -contains $env:GLIFF_DIR)) {
    if ($path) { $path = "$path;$env:GLIFF_DIR" } else { $path = $env:GLIFF_DIR }
    [Environment]::SetEnvironmentVariable('Path', $path, 'User')
}
"#;
    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            SCRIPT,
        ])
        .env("GLIFF_LNK", &shortcut)
        .env("GLIFF_EXE", &target)
        .env("GLIFF_DIR", &dir)
        .status()
        .context("run PowerShell")?;
    anyhow::ensure!(
        status.success(),
        "PowerShell could not create the shortcut ({status})"
    );
    println!("Installed {}", target.display());
    println!("Start menu: \"gliff\" opens full screen to the last machine you connected to.");
    println!("Terminal: open a new one and run `gliff-win user@host`.");
    Ok(())
}
