// GUI-subsystem autostart launcher for the sociacli daemon.
//
// Why a separate binary? `sociacli.exe` is a *console* program (the REPL needs
// a terminal), so launching `sociacli listen` from the HKCU\...\Run key makes
// Windows pop a black console window at every login. The previous fix used a
// `.vbs` run by `wscript.exe`, but a script host launching a hidden process
// from a Run key is a classic malware pattern and antivirus heuristics flag it.
//
// This launcher sidesteps both problems: it is built as a GUI-subsystem binary
// (`#![windows_subsystem = "windows"]`), so Windows never allocates a console
// for it, and it spawns `sociacli listen` detached with CREATE_NO_WINDOW so the
// daemon has no console either. It is an ordinary signed-the-same-way .exe, not
// a script, so it doesn't trip the wscript heuristics.
#![windows_subsystem = "windows"]

fn main() {
    // Run the sociacli binary sitting next to this launcher.
    let sibling = if cfg!(windows) { "sociacli.exe" } else { "sociacli" };
    let Some(target) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(sibling)))
    else {
        return;
    };

    let mut cmd = std::process::Command::new(target);
    cmd.arg("listen")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NO_WINDOW — the daemon runs with no console
        // and outlives this launcher.
        cmd.creation_flags(0x0000_0008 | 0x0800_0000);
    }

    let _ = cmd.spawn();
}
