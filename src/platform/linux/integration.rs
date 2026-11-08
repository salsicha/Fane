//! Linux desktop integration: XDG autostart, the GNOME helper extension, and a GNOME
//! custom keyboard shortcut.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

const EXT_UUID: &str = "fane-helper@salsicha.github.io";
const SHORTCUT_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/fane/";
const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";

/// The running executable, quoted if needed for a desktop-entry `Exec=` / shortcut command.
fn exe_command() -> anyhow::Result<String> {
    let exe = std::env::current_exe().context("cannot find the fane executable")?;
    let exe = exe.to_string_lossy().into_owned();
    Ok(if exe.contains(char::is_whitespace) { format!("\"{exe}\"") } else { exe })
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Start Fane at login (or stop). Returns a human-readable description of what was done.
pub fn set_autostart(on: bool) -> anyhow::Result<String> {
    let path = dirs::config_dir().unwrap_or_else(|| home().join(".config")).join("autostart/fane.desktop");
    if !on {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        }
        return Ok("Autostart disabled.".into());
    }
    let exe = exe_command()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(
        &path,
        format!(
            "[Desktop Entry]\nType=Application\nName=Fane\nExec={exe} run\n\
             X-GNOME-Autostart-enabled=true\nNoDisplay=true\n"
        ),
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(format!("Autostart enabled ({}).", path.display()))
}

/// Remove a file, symlink or directory tree if present. Returns whether anything was removed.
fn remove_any(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path).map(|()| true),
        Ok(_) => std::fs::remove_file(path).map(|()| true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn copy_dir(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn enable_extension() -> bool {
    Command::new("gnome-extensions").args(["enable", EXT_UUID]).output().is_ok_and(|o| o.status.success())
}

fn enable_message(enabled: bool) -> String {
    if enabled {
        "Enabled. If `fane doctor` still can't see it, log out and back in.".into()
    } else {
        format!(
            "On Wayland GNOME only loads new extensions after you log out and back in.\n\
             Then run: gnome-extensions enable {EXT_UUID}"
        )
    }
}

/// Linux: install the bundled GNOME Shell helper extension. Returns a message for the user.
pub fn install_extension() -> anyhow::Result<String> {
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("gnome-extension").join(EXT_UUID);
    let system = Path::new("/usr/share/gnome-shell/extensions").join(EXT_UUID);
    let user = home().join(".local/share/gnome-shell/extensions").join(EXT_UUID);

    if dev.is_dir() {
        // Development checkout: copy it into the user's extensions (replacing any old copy).
        remove_any(&user).with_context(|| format!("removing {}", user.display()))?;
        copy_dir(&dev, &user).with_context(|| format!("copying {} to {}", dev.display(), user.display()))?;
        let enabled = enable_extension();
        return Ok(format!("Installed {EXT_UUID} to {}.\n{}", user.display(), enable_message(enabled)));
    }
    if system.is_dir() {
        // Installed by the package: a user copy would shadow it, so drop any stale one.
        let removed = remove_any(&user).with_context(|| format!("removing {}", user.display()))?;
        let enabled = enable_extension();
        let mut msg = format!("Using the packaged {EXT_UUID} ({}).", system.display());
        if removed {
            msg.push_str(&format!(" Removed the old copy in {}.", user.display()));
        }
        return Ok(format!("{msg}\n{}", enable_message(enabled)));
    }
    bail!("Could not find the gnome-extension directory.")
}

fn gsettings(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("gsettings").args(args).output().context("could not run gsettings")?;
    if !out.status.success() {
        bail!("gsettings {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Parse a GVariant string array as printed by gsettings: `@as []` or `['a', "b"]`.
fn parse_strv(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    let raw = raw.strip_prefix("@as").unwrap_or(raw).trim();
    let inner = raw.strip_prefix('[').and_then(|r| r.strip_suffix(']')).unwrap_or("");
    let mut out = vec![];
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\'' && c != '"' {
            continue; // separators and whitespace between items
        }
        let quote = c;
        let mut item = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => item.extend(chars.next()),
                c if c == quote => break,
                c => item.push(c),
            }
        }
        out.push(item);
    }
    out
}

fn format_strv(items: &[String]) -> String {
    let quoted: Vec<String> =
        items.iter().map(|s| format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))).collect();
    format!("[{}]", quoted.join(", "))
}

/// Linux: bind a GNOME custom shortcut to `<exe> toggle`. Returns a message for the user.
pub fn setup_shortcut(binding: &str) -> anyhow::Result<String> {
    let exe = exe_command()?;
    let mut current = parse_strv(&gsettings(&["get", MEDIA_KEYS, "custom-keybindings"])?);
    if !current.iter().any(|p| p == SHORTCUT_PATH) {
        current.push(SHORTCUT_PATH.to_string());
        gsettings(&["set", MEDIA_KEYS, "custom-keybindings", &format_strv(&current)])?;
    }
    let schema = format!("{MEDIA_KEYS}.custom-keybinding:{SHORTCUT_PATH}");
    gsettings(&["set", &schema, "name", "Fane"])?;
    gsettings(&["set", &schema, "command", &format!("{exe} toggle")])?;
    gsettings(&["set", &schema, "binding", binding])?;
    Ok(format!("Bound {binding} → `{exe} toggle`. Change it in Settings → Keyboard → Custom Shortcuts."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strv_round_trip() {
        assert!(parse_strv("@as []").is_empty());
        assert!(parse_strv("[]").is_empty());
        let v = parse_strv("['/org/a/custom0/', \"/org/b/it's/\"]");
        assert_eq!(v, vec!["/org/a/custom0/".to_string(), "/org/b/it's/".to_string()]);
        assert_eq!(parse_strv(&format_strv(&v)), v);
        assert_eq!(format_strv(&["/x/".into()]), "['/x/']");
    }

    #[test]
    fn bundled_extension_exists_in_checkout() {
        let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("gnome-extension").join(EXT_UUID);
        assert!(dev.join("extension.js").is_file());
        assert!(dev.join("metadata.json").is_file());
    }
}
