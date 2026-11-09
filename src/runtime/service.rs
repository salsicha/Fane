//! Running Fane's model server as a background service (`fane serve`), so other computers can
//! use this one's GPU without Fane's window being open: a systemd user unit on Linux, a launchd
//! agent on macOS, and a sign-in entry on Windows.

use std::path::PathBuf;

fn fane_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("can't find Fane's program: {e}"))
}

#[cfg(target_os = "linux")]
fn unit_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config")).join("systemd/user/fane-serve.service")
}

#[cfg(target_os = "macos")]
fn plist_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("Library/LaunchAgents/com.fane.serve.plist")
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(program).args(args).output().map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{program} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

pub fn installed() -> bool {
    #[cfg(target_os = "linux")]
    return unit_path().is_file();
    #[cfg(target_os = "macos")]
    return plist_path().is_file();
    #[cfg(windows)]
    return windows::installed();
    #[allow(unreachable_code)]
    false
}

/// Install and start the service. Returns a note for the user.
pub fn install() -> Result<String, String> {
    let exe = fane_exe()?;
    #[cfg(target_os = "linux")]
    {
        let unit = format!(
            "[Unit]\nDescription=Fane model server (llama.cpp and stable-diffusion.cpp)\nAfter=network-online.target\n\n\
             [Service]\nExecStart=\"{}\" serve\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
            exe.display()
        );
        let path = unit_path();
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&path, unit).map_err(|e| format!("can't write {}: {e}", path.display()))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", "--now", "fane-serve.service"])?;
        // Keep it running when you're signed out (may need an administrator; not essential).
        let user = crate::config::whoami();
        let linger = run("loginctl", &["enable-linger", &user]).is_ok();
        return Ok(if linger {
            "Fane's model server now runs in the background, also while you're signed out.".into()
        } else {
            "Fane's model server now runs in the background while you're signed in.".into()
        });
    }
    #[cfg(target_os = "macos")]
    {
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.fane.serve</string>
<key>ProgramArguments</key><array><string>{}</string><string>serve</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
</dict></plist>
"#,
            exe.display()
        );
        let path = plist_path();
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&path, plist).map_err(|e| e.to_string())?;
        let _ = run("launchctl", &["unload", &path.display().to_string()]);
        run("launchctl", &["load", "-w", &path.display().to_string()])?;
        return Ok("Fane's model server now runs in the background while you're signed in.".into());
    }
    #[cfg(windows)]
    {
        windows::install(&exe)?;
        return Ok("Fane's model server now runs in the background while you're signed in.".into());
    }
    #[allow(unreachable_code)]
    Err("background service isn't supported on this system".into())
}

pub fn uninstall() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let _ = run("systemctl", &["--user", "disable", "--now", "fane-serve.service"]);
        let _ = std::fs::remove_file(unit_path());
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    #[cfg(target_os = "macos")]
    {
        let path = plist_path();
        let _ = run("launchctl", &["unload", "-w", &path.display().to_string()]);
        let _ = std::fs::remove_file(path);
    }
    #[cfg(windows)]
    windows::uninstall()?;
    Ok(())
}

#[cfg(windows)]
mod windows {
    use std::path::Path;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn installed() -> bool {
        super::run("reg", &["query", KEY, "/v", "FaneServe"]).is_ok()
    }

    pub fn install(exe: &Path) -> Result<(), String> {
        let value = format!("\"{}\" serve", exe.display());
        super::run("reg", &["add", KEY, "/v", "FaneServe", "/t", "REG_SZ", "/d", &value, "/f"])?;
        // Start it now too (fane.exe is a windowless program).
        std::process::Command::new(exe).arg("serve").spawn().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn uninstall() -> Result<(), String> {
        let _ = super::run("reg", &["delete", KEY, "/v", "FaneServe", "/f"]);
        Ok(())
    }
}
