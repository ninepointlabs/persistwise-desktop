//! "Keep checking feeds when Persistwise is closed."
//!
//! On Linux with systemd this installs a *user* timer (no root, no service we
//! run) that starts `persistwise-desktop --poll` every few minutes: it checks
//! feeds once and exits. Turning it off removes exactly those two files.
//! Not available in the Flatpak (no access to the host's systemd).
//!
//! On Windows the same switch keeps Persistwise running in the tray when the
//! window is closed and starts it hidden at sign-in (`--background`): the
//! MSIX declares a startup task for that, and the plain .exe uses the
//! per-user Run registry key. The switch is the `feeds.background` setting;
//! a `--background` launch with it off exits straight away.

use serde::Serialize;
use std::path::{Path, PathBuf};

pub const UNIT: &str = "com.ninepointlabs.Persistwise-poll";

#[derive(Serialize, Debug, Clone)]
pub struct Status {
    pub supported: bool,
    pub enabled: bool,
    /// Why it is not available, in plain words.
    pub reason: Option<String>,
    /// The command the timer runs.
    pub command: Option<String>,
}

fn unit_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("systemd/user"))
}

/// The program the timer should start: the AppImage file itself when we run
/// from one, otherwise this executable.
fn program() -> Option<String> {
    if let Some(ai) = std::env::var_os("APPIMAGE").filter(|a| !a.is_empty()) {
        return Some(PathBuf::from(ai).display().to_string());
    }
    std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
}

fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || std::path::Path::new("/.flatpak-info").exists()
}

fn has_systemctl() -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("systemctl").is_file()))
        .unwrap_or(false)
}

pub fn status(root: &Path) -> Status {
    let unsupported = |reason: &str| Status {
        supported: false,
        enabled: false,
        reason: Some(reason.into()),
        command: None,
    };
    if cfg!(windows) {
        let enabled = persistwise_core::settings::load(root).feeds.background;
        return Status {
            supported: true,
            enabled,
            reason: None,
            command: program().map(|p| format!("\"{p}\" --background")),
        };
    }
    if !cfg!(target_os = "linux") {
        return unsupported("Not available on this system.");
    }
    if in_flatpak() {
        return unsupported("The Flatpak cannot set up a background timer. Feeds are checked while Persistwise is open (the AppImage can do this).");
    }
    if !has_systemctl() {
        return unsupported("This system does not use systemd. You can run `persistwise-desktop --poll` from cron instead.");
    }
    let Some(dir) = unit_dir() else {
        return unsupported("Could not find your config folder.");
    };
    let enabled = dir.join(format!("{UNIT}.timer")).is_file();
    Status {
        supported: true,
        enabled,
        reason: None,
        command: program().map(|p| format!("\"{p}\" --poll")),
    }
}

/// True for the Microsoft Store (MSIX) install, which lives under WindowsApps.
pub fn is_msix() -> bool {
    std::env::current_exe()
        .map(|p| {
            p.to_string_lossy()
                .to_ascii_lowercase()
                .contains("\\windowsapps\\")
        })
        .unwrap_or(false)
}

/// Windows: the per-user "run at sign-in" entry for the plain .exe. The MSIX
/// uses its declared startup task instead (registry writes are virtualised
/// inside a package and would not take effect).
fn windows_run_key(enable: bool) -> Result<(), String> {
    if is_msix() {
        return Ok(());
    }
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    let mut cmd = std::process::Command::new("reg");
    if enable {
        let program = program().ok_or("could not find the Persistwise program")?;
        cmd.args([
            "add",
            KEY,
            "/v",
            "Persistwise",
            "/t",
            "REG_SZ",
            "/d",
            &format!("\"{program}\" --background"),
            "/f",
        ]);
    } else {
        cmd.args(["delete", KEY, "/v", "Persistwise", "/f"]);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("could not run reg: {e}"))?;
    if out.status.success() || !enable {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Windows: make sure the sign-in entry matches the setting (called at start).
pub fn sync_windows(root: &Path) {
    if cfg!(windows) {
        let _ = windows_run_key(persistwise_core::settings::load(root).feeds.background);
    }
}

fn set_windows_flag(root: &Path, on: bool) -> Result<Status, String> {
    let mut s = persistwise_core::settings::load(root);
    s.feeds.background = on;
    persistwise_core::settings::save(root, &s).map_err(|e| e.to_string())?;
    windows_run_key(on)?;
    Ok(status(root))
}

/// The `PERSISTWISE_DATA_DIR` the timer runs with: the one the app itself was
/// started with, else the archive folder when it is not the default one (so
/// the timer keeps using the folder chosen in Settings even if the pointer
/// file goes missing). None for the default folder, as before.
pub fn timer_data_dir(root: &Path, env: Option<String>, default: Option<&Path>) -> Option<String> {
    if let Some(e) = env.filter(|e| !e.is_empty()) {
        return Some(e);
    }
    match default {
        Some(d) if persistwise_core::location::same_dir(root, d) => None,
        _ => Some(root.display().to_string()),
    }
}

/// systemd unit files for a check every `minutes`.
pub fn unit_files(program: &str, minutes: u32, data_dir: Option<&str>) -> (String, String) {
    let quoted = program.replace('\\', "\\\\").replace('"', "\\\"");
    let env = data_dir
        .map(|d| {
            format!(
                "Environment=\"PERSISTWISE_DATA_DIR={}\"\n",
                d.replace('"', "\\\"")
            )
        })
        .unwrap_or_default();
    let service = format!(
        "[Unit]\nDescription=Persistwise: check feeds and archive new posts\nAfter=network-online.target\n\n\
         [Service]\nType=oneshot\nExecStart=\"{quoted}\" --poll\n{env}Nice=10\nIOSchedulingClass=idle\n"
    );
    let calendar = match minutes {
        0 | 60.. => "hourly".to_string(),
        m => format!("*:0/{m}"),
    };
    let timer = format!(
        "[Unit]\nDescription=Persistwise: check feeds every {minutes} minutes\n\n\
         [Timer]\nOnCalendar={calendar}\nPersistent=true\nRandomizedDelaySec=60\n\n[Install]\nWantedBy=timers.target\n"
    );
    (service, timer)
}

fn systemctl(args: &[&str]) -> Result<(), String> {
    let out = crate::hostenv::host_command("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("could not run systemctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn enable(root: &Path, minutes: u32) -> Result<Status, String> {
    if cfg!(windows) {
        return set_windows_flag(root, true);
    }
    let st = status(root);
    if !st.supported {
        return Err(st.reason.unwrap_or_default());
    }
    let dir = unit_dir().ok_or("no config folder")?;
    let program = program().ok_or("could not find the Persistwise program")?;
    let data_dir = timer_data_dir(
        root,
        std::env::var(persistwise_core::location::ENV_VAR).ok(),
        persistwise_core::location::default_dir().ok().as_deref(),
    );
    let (service, timer) = unit_files(&program, minutes.max(5), data_dir.as_deref());
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{UNIT}.service")), service).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(format!("{UNIT}.timer")), timer).map_err(|e| e.to_string())?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", &format!("{UNIT}.timer")])?;
    Ok(status(root))
}

pub fn disable(root: &Path) -> Result<Status, String> {
    if cfg!(windows) {
        return set_windows_flag(root, false);
    }
    let dir = unit_dir().ok_or("no config folder")?;
    let _ = systemctl(&["disable", "--now", &format!("{UNIT}.timer")]);
    for ext in ["timer", "service"] {
        let f = dir.join(format!("{UNIT}.{ext}"));
        if f.exists() {
            std::fs::remove_file(f).map_err(|e| e.to_string())?;
        }
    }
    let _ = systemctl(&["daemon-reload"]);
    Ok(status(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_files_quote_paths_and_set_the_schedule() {
        let (service, timer) =
            unit_files("/home/u/Apps/Persist wise.AppImage", 15, Some("/data/pw"));
        assert!(service.contains("ExecStart=\"/home/u/Apps/Persist wise.AppImage\" --poll"));
        assert!(service.contains("Environment=\"PERSISTWISE_DATA_DIR=/data/pw\""));
        assert!(service.contains("Type=oneshot"));
        assert!(timer.contains("OnCalendar=*:0/15"));
        assert!(timer.contains("Persistent=true"));
        assert!(unit_files("/x", 60, None).1.contains("OnCalendar=hourly"));
        assert!(!unit_files("/x", 30, None).0.contains("Environment="));
    }

    #[test]
    fn timer_follows_a_chosen_archive_folder() {
        let default = Path::new("/home/u/.local/share/persistwise");
        let chosen = Path::new("/media/u/Archive Drive/Persistwise");
        // Default folder: no override, exactly as before.
        assert_eq!(timer_data_dir(default, None, Some(default)), None);
        // A folder chosen in Settings is passed to the timer.
        let dir = timer_data_dir(chosen, None, Some(default));
        assert_eq!(dir.as_deref(), Some("/media/u/Archive Drive/Persistwise"));
        let (service, _) = unit_files("/opt/pw/persistwise-desktop", 15, dir.as_deref());
        assert!(
            service.contains(
                "Environment=\"PERSISTWISE_DATA_DIR=/media/u/Archive Drive/Persistwise\""
            ),
            "{service}"
        );
        // PERSISTWISE_DATA_DIR given to the app still wins.
        assert_eq!(
            timer_data_dir(chosen, Some("/portable".into()), Some(default)).as_deref(),
            Some("/portable")
        );
        assert_eq!(
            timer_data_dir(chosen, Some(String::new()), Some(default)).as_deref(),
            Some("/media/u/Archive Drive/Persistwise")
        );
    }

    #[test]
    fn systemd_accepts_the_units() {
        // Only where systemd-analyze exists; it reads the files, installs nothing.
        let Some(sa) = ["/usr/bin/systemd-analyze", "/bin/systemd-analyze"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
        else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("pw-units-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (service, timer) = unit_files("/bin/true", 15, None);
        std::fs::write(dir.join(format!("{UNIT}.service")), service).unwrap();
        std::fs::write(dir.join(format!("{UNIT}.timer")), timer).unwrap();
        let out = std::process::Command::new(sa)
            .arg("verify")
            .arg("--user")
            .arg(dir.join(format!("{UNIT}.service")))
            .arg(dir.join(format!("{UNIT}.timer")))
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stderr}");
        assert!(!stderr.contains(UNIT), "{stderr}");
    }
}
