//! Omarchy theme follow, with graceful fallback.
//!
//! On Omarchy the current theme's `colors.toml` is read and handed to the UI
//! as CSS colours. Anywhere else (other distros, Windows) this returns None
//! and the UI uses its own light/dark palette via `prefers-color-scheme`.
//! Nothing is written, and no file watcher is started on missing paths: the
//! UI simply asks again every few seconds while Omarchy is present.

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ThemeColors {
    pub mode: String,
    pub colors: HashMap<String, String>,
    /// Changes whenever the theme changes, so the UI can skip re-applying.
    pub generation: String,
}

pub fn omarchy_theme_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/state"));
    [
        state.join("omarchy/current/theme"),
        home.join(".config/omarchy/current/theme"),
    ]
    .into_iter()
    .find(|p| p.join("colors.toml").is_file())
}

pub fn read() -> Option<ThemeColors> {
    let dir = omarchy_theme_dir()?;
    let text = std::fs::read_to_string(dir.join("colors.toml")).ok()?;
    let mut colors = HashMap::new();
    let mut mode = String::from("dark");
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('#') || !l.contains('=') {
            continue;
        }
        let (k, v) = l.split_once('=').unwrap();
        let k = k.trim();
        let v = v.trim().trim_matches('"').trim();
        if k == "mode" {
            mode = v.to_string();
        } else if is_hex_colour(v) && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            colors.insert(k.to_string(), v.to_string());
        }
    }
    if colors.is_empty() {
        return None;
    }
    let generation = std::fs::read_to_string(dir.parent()?.join("theme.generation"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let generation = if generation.is_empty() {
        std::fs::metadata(dir.join("colors.toml"))
            .and_then(|m| m.modified())
            .map(|t| format!("{t:?}"))
            .unwrap_or_default()
    } else {
        generation
    };
    Some(ThemeColors {
        mode,
        colors,
        generation,
    })
}

fn is_hex_colour(v: &str) -> bool {
    let h = v.strip_prefix('#').unwrap_or("");
    matches!(h.len(), 3 | 6 | 8) && h.chars().all(|c| c.is_ascii_hexdigit())
}
