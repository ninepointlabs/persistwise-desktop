//! Starting other programs (xdg-open) with the host's environment.
//!
//! Inside the AppImage the launcher points LD_LIBRARY_PATH, GTK_PATH,
//! GIO_MODULE_DIR and friends at the bundled libraries. A browser or file
//! manager started with those would load the wrong libraries and may crash.
//! The AppImage's AppRun saves the original values as PERSISTWISE_HOST_<VAR>;
//! here they are put back (or removed) for the child only.

use std::process::Command;

/// Variables the bundled launcher may change.
pub const MANAGED: &[&str] = &[
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "PATH",
    "XDG_DATA_DIRS",
    "GTK_DATA_PREFIX",
    "GTK_THEME",
    "GTK_EXE_PREFIX",
    "GTK_PATH",
    "GTK_IM_MODULE_FILE",
    "GDK_PIXBUF_MODULE_FILE",
    "GDK_BACKEND",
    "GSETTINGS_SCHEMA_DIR",
    "GI_TYPELIB_PATH",
    "GIO_MODULE_DIR",
    "GIO_EXTRA_MODULES",
    "PYTHONHOME",
    "PYTHONPATH",
    "PERLLIB",
    "QT_PLUGIN_PATH",
    "WEBKIT_DISABLE_COMPOSITING_MODE",
];

/// A Command that runs with the host environment restored when we are
/// inside the AppImage, and unchanged otherwise.
pub fn host_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    if std::env::var_os("PERSISTWISE_HOST_ENV_SAVED").is_some() {
        for v in MANAGED {
            match std::env::var_os(format!("PERSISTWISE_HOST_{v}")) {
                Some(orig) => {
                    cmd.env(v, orig);
                }
                None => {
                    cmd.env_remove(v);
                }
            }
        }
        for (k, _) in std::env::vars_os() {
            if let Some(k) = k.to_str() {
                if k.starts_with("PERSISTWISE_HOST_") || k == "APPDIR" || k == "APPIMAGE_GTK_THEME"
                {
                    cmd.env_remove(k);
                }
            }
        }
    }
    cmd
}

/// Opens a URL or path with the desktop's default handler.
#[cfg(target_os = "linux")]
pub fn xdg_open(target: &str) -> std::io::Result<()> {
    let mut cmd = host_command("xdg-open");
    cmd.arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn()?;
    // Reap it in the background so it never lingers as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)] // runs the Unix `env` tool; this only matters for the Linux AppImage
    fn restores_host_values_for_children_only() {
        // Simulates what the AppImage AppRun sets up. This is the only test
        // in this crate that touches the process environment.
        std::env::set_var("PERSISTWISE_HOST_ENV_SAVED", "1");
        std::env::set_var("PERSISTWISE_HOST_PATH", "/usr/bin:/bin");
        std::env::set_var("PATH", "/tmp/.mount_x/usr/bin:/usr/bin:/bin");
        std::env::set_var("GTK_PATH", "/tmp/.mount_x/usr/lib/gtk-3.0");
        std::env::remove_var("PERSISTWISE_HOST_GTK_PATH");
        let out = host_command("env").output().expect("run env");
        let env = String::from_utf8_lossy(&out.stdout);
        assert!(env.lines().any(|l| l == "PATH=/usr/bin:/bin"), "{env}");
        assert!(!env.lines().any(|l| l.starts_with("GTK_PATH=")), "{env}");
        assert!(!env.contains("PERSISTWISE_HOST_"), "{env}");
    }
}
