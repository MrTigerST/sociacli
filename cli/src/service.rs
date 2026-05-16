// Cross-platform "start sociacli listen at user login" setup.
//
//   Windows  → HKCU\Software\Microsoft\Windows\CurrentVersion\Run\sociacli
//              (same key the Inno installer writes — idempotent).
//   Linux    → ~/.config/systemd/user/sociacli.service + `systemctl --user
//              enable --now sociacli`.
//   macOS    → ~/Library/LaunchAgents/dev.sociacli.daemon.plist + `launchctl
//              load -w`.
//
// All paths are per-user; no root / admin required.

use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().context("current_exe")
}

pub fn install() -> Result<String> {
    let exe = current_exe()?;
    #[cfg(target_os = "windows")]
    return install_windows(&exe);
    #[cfg(target_os = "linux")]
    return install_linux(&exe);
    #[cfg(target_os = "macos")]
    return install_macos(&exe);
    #[allow(unreachable_code)]
    {
        let _ = exe;
        anyhow::bail!("autostart not implemented on this platform")
    }
}

pub fn uninstall() -> Result<String> {
    #[cfg(target_os = "windows")]
    return uninstall_windows();
    #[cfg(target_os = "linux")]
    return uninstall_linux();
    #[cfg(target_os = "macos")]
    return uninstall_macos();
    #[allow(unreachable_code)]
    anyhow::bail!("autostart not implemented on this platform")
}

pub fn status() -> Result<String> {
    #[cfg(target_os = "windows")]
    return status_windows();
    #[cfg(target_os = "linux")]
    return status_linux();
    #[cfg(target_os = "macos")]
    return status_macos();
    #[allow(unreachable_code)]
    Ok("unknown".into())
}

// ---------------- Windows ----------------

/// Path of the GUI-subsystem launcher (`sociacli-launch.exe`) shipped next to
/// the main binary. The autostart entry points at this instead of the console
/// `sociacli.exe` so the boot daemon comes up with no console window — and,
/// being an ordinary .exe rather than a script, it doesn't trip the antivirus
/// heuristics that flag `wscript.exe foo.vbs` in a Run key. Falls back to the
/// bare exe if the launcher isn't alongside us (e.g. a bare `cargo run` build).
#[cfg(target_os = "windows")]
fn autostart_command(exe: &std::path::Path) -> String {
    if let Some(launch) = exe.parent().map(|d| d.join("sociacli-launch.exe")) {
        if launch.exists() {
            return format!("\"{}\"", launch.display());
        }
    }
    format!("\"{}\" listen", exe.display())
}

#[cfg(target_os = "windows")]
fn install_windows(exe: &std::path::Path) -> Result<String> {
    use std::process::Command;
    let value = autostart_command(exe);
    let out = Command::new("reg")
        .args([
            "add",
            "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            "/v",
            "sociacli",
            "/t",
            "REG_SZ",
            "/d",
            &value,
            "/f",
        ])
        .output()
        .context("invoke reg.exe")?;
    if !out.status.success() {
        anyhow::bail!(
            "reg add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(format!("registered HKCU\\…\\Run\\sociacli = {value}"))
}

#[cfg(target_os = "windows")]
fn uninstall_windows() -> Result<String> {
    use std::process::Command;
    let _ = Command::new("reg")
        .args([
            "delete",
            "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            "/v",
            "sociacli",
            "/f",
        ])
        .output();
    Ok("removed HKCU\\…\\Run\\sociacli (if present)".into())
}

#[cfg(target_os = "windows")]
fn status_windows() -> Result<String> {
    use std::process::Command;
    let out = Command::new("reg")
        .args([
            "query",
            "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            "/v",
            "sociacli",
        ])
        .output()
        .context("invoke reg.exe")?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().into())
    } else {
        Ok("not registered (run `sociacli service install`)".into())
    }
}

// ---------------- Linux ----------------

#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf> {
    let dir = directories::BaseDirs::new()
        .context("no HOME")?
        .config_dir()
        .join("systemd/user");
    std::fs::create_dir_all(&dir).ok();
    Ok(dir.join("sociacli.service"))
}

#[cfg(target_os = "linux")]
fn install_linux(exe: &std::path::Path) -> Result<String> {
    let unit = unit_path()?;
    let body = format!(
        "[Unit]\n\
         Description=sociacli notification + P2P daemon\n\
         After=network.target\n\
         \n\
         [Service]\n\
         ExecStart={exe} listen\n\
         Restart=on-failure\n\
         RestartSec=3\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exe.display()
    );
    std::fs::write(&unit, body).with_context(|| format!("write {}", unit.display()))?;
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let s = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "sociacli"])
        .status()
        .context("invoke systemctl")?;
    if !s.success() {
        anyhow::bail!(
            "systemctl --user enable --now sociacli failed (exit {:?})",
            s.code()
        );
    }
    Ok(format!("installed {} + enabled", unit.display()))
}

#[cfg(target_os = "linux")]
fn uninstall_linux() -> Result<String> {
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "sociacli"])
        .status();
    if let Ok(p) = unit_path() {
        let _ = std::fs::remove_file(p);
    }
    Ok("disabled + removed user unit".into())
}

#[cfg(target_os = "linux")]
fn status_linux() -> Result<String> {
    let out = std::process::Command::new("systemctl")
        .args(["--user", "is-enabled", "sociacli"])
        .output()
        .context("invoke systemctl")?;
    let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(if state.is_empty() { "not installed".into() } else { state })
}

// ---------------- macOS ----------------

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("no HOME")?;
    let dir = PathBuf::from(home).join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).ok();
    Ok(dir.join("dev.sociacli.daemon.plist"))
}

#[cfg(target_os = "macos")]
fn install_macos(exe: &std::path::Path) -> Result<String> {
    let path = plist_path()?;
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.sociacli.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>listen</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/tmp/sociacli.out.log</string>
  <key>StandardErrorPath</key><string>/tmp/sociacli.err.log</string>
</dict>
</plist>
"#,
        exe = exe.display()
    );
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    let _ = std::process::Command::new("launchctl")
        .args(["unload", path.to_string_lossy().as_ref()])
        .status();
    let s = std::process::Command::new("launchctl")
        .args(["load", "-w", path.to_string_lossy().as_ref()])
        .status()
        .context("invoke launchctl")?;
    if !s.success() {
        anyhow::bail!("launchctl load failed (exit {:?})", s.code());
    }
    Ok(format!("installed {} + loaded", path.display()))
}

#[cfg(target_os = "macos")]
fn uninstall_macos() -> Result<String> {
    if let Ok(p) = plist_path() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", p.to_string_lossy().as_ref()])
            .status();
        let _ = std::fs::remove_file(p);
    }
    Ok("unloaded + removed LaunchAgent".into())
}

#[cfg(target_os = "macos")]
fn status_macos() -> Result<String> {
    let out = std::process::Command::new("launchctl")
        .args(["list", "dev.sociacli.daemon"])
        .output()
        .context("invoke launchctl")?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().into())
    } else {
        Ok("not loaded (run `sociacli service install`)".into())
    }
}
