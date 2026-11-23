//! User settings. Non-secret settings live in `settings.json`; keys the user
//! pastes in live in `secrets.json` (owner-only permissions on Unix) and are
//! never sent back to the UI except as the last four characters.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Send saved pages to the Wayback Machine (public Save Page Now).
    /// On by default; it is what Persistwise is for. Feed pages follow it too.
    pub wayback_on_save: bool,
    pub ipfs: IpfsSettings,
    pub feeds: FeedSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            wayback_on_save: true,
            ipfs: IpfsSettings::default(),
            feeds: FeedSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FeedSettings {
    /// How often feeds are checked while the app is open. 0 turns it off.
    pub poll_minutes: u32,
}

impl Default for FeedSettings {
    fn default() -> Self {
        Self { poll_minutes: 15 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum IpfsMode {
    /// IPFS is not set up. The default.
    #[default]
    Off,
    /// A Kubo-compatible RPC API: IPFS Desktop or Kubo on this machine, or a
    /// hosted Kubo-compatible RPC with the user's own token.
    Kubo,
    /// The user's own Pinata account (JWT).
    Pinata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct IpfsSettings {
    pub mode: IpfsMode,
    /// Kubo RPC API address. IPFS Desktop and Kubo listen here by default.
    pub kubo_api_url: String,
    /// Gateway used to build IPFS links. Empty means a sensible default for
    /// the mode (local gateway for Kubo, a public gateway otherwise).
    pub gateway: String,
    /// Pin each new save automatically (when IPFS is set up).
    pub pin_on_save: bool,
}

impl Default for IpfsSettings {
    fn default() -> Self {
        Self {
            mode: IpfsMode::Off,
            kubo_api_url: "http://127.0.0.1:5001".into(),
            gateway: String::new(),
            pin_on_save: true,
        }
    }
}

impl IpfsSettings {
    pub fn gateway_or_default(&self) -> String {
        let g = self.gateway.trim().trim_end_matches('/');
        if !g.is_empty() {
            return g.to_string();
        }
        match self.mode {
            IpfsMode::Kubo
                if self.kubo_api_url.contains("127.0.0.1")
                    || self.kubo_api_url.contains("localhost") =>
            {
                "http://127.0.0.1:8080".into()
            }
            _ => "https://ipfs.filebase.io".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Secrets {
    /// Optional bearer token for a hosted Kubo-compatible RPC.
    pub kubo_token: Option<String>,
    pub pinata_jwt: Option<String>,
}

pub fn load(root: &Path) -> Settings {
    std::fs::read(root.join("settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(root: &Path, s: &Settings) -> Result<()> {
    write_atomic(
        &root.join("settings.json"),
        &serde_json::to_vec_pretty(s)?,
        false,
    )
}

pub fn load_secrets(root: &Path) -> Secrets {
    std::fs::read(root.join("secrets.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_secrets(root: &Path, s: &Secrets) -> Result<()> {
    write_atomic(
        &root.join("secrets.json"),
        &serde_json::to_vec_pretty(s)?,
        true,
    )
}

fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let tmp = path.with_extension("json.part");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        use std::io::Write;
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), Settings::default());
        assert!(
            Settings::default().wayback_on_save,
            "Wayback is on by default"
        );
        let s = Settings {
            wayback_on_save: false,
            ipfs: IpfsSettings {
                mode: IpfsMode::Kubo,
                ..Default::default()
            },
            feeds: FeedSettings { poll_minutes: 30 },
        };
        save(dir.path(), &s).unwrap();
        assert_eq!(load(dir.path()), s);
        assert_eq!(s.ipfs.gateway_or_default(), "http://127.0.0.1:8080");

        save_secrets(
            dir.path(),
            &Secrets {
                pinata_jwt: Some("abc".into()),
                kubo_token: None,
            },
        )
        .unwrap();
        assert_eq!(load_secrets(dir.path()).pinata_jwt.as_deref(), Some("abc"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("secrets.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
