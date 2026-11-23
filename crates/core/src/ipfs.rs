//! Optional IPFS pinning, set up by the user. Nine Point Labs never pays for
//! or provides pinning. Two ways in:
//!
//! * Kubo RPC: IPFS Desktop or Kubo running on this machine (default
//!   `http://127.0.0.1:5001`), or any hosted Kubo-compatible RPC with the
//!   user's own bearer token.
//! * Pinata, with the user's own JWT.

use anyhow::{anyhow, bail, Result};
use std::path::Path;
use std::time::Duration;

use crate::settings::{IpfsMode, IpfsSettings, Secrets};

pub const PINATA_UPLOAD_URL: &str = "https://uploads.pinata.cloud/v3/files";
pub const PINATA_AUTH_URL: &str = "https://api.pinata.cloud/data/testAuthentication";

#[derive(Debug, Clone)]
pub struct Pinned {
    pub cid: String,
    pub service: &'static str,
}

/// Endpoints, overridable in tests.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub pinata_upload: String,
    pub pinata_auth: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self { pinata_upload: PINATA_UPLOAD_URL.into(), pinata_auth: PINATA_AUTH_URL.into() }
    }
}

pub fn is_configured(s: &IpfsSettings, secrets: &Secrets) -> bool {
    match s.mode {
        IpfsMode::Off => false,
        IpfsMode::Kubo => !s.kubo_api_url.trim().is_empty(),
        IpfsMode::Pinata => secrets.pinata_jwt.as_deref().is_some_and(|j| !j.trim().is_empty()),
    }
}

fn client() -> Result<reqwest::blocking::Client> {
    crate::http::client(Duration::from_secs(120))
}

fn kubo_base(s: &IpfsSettings) -> String {
    s.kubo_api_url.trim().trim_end_matches('/').to_string()
}

fn with_kubo_auth(req: reqwest::blocking::RequestBuilder, secrets: &Secrets) -> reqwest::blocking::RequestBuilder {
    match secrets.kubo_token.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(t) => req.bearer_auth(t.trim()),
        None => req,
    }
}

/// Checks the configured service answers and the credentials work.
/// Returns a short human description, e.g. "Kubo 0.38.1".
pub fn test_connection(s: &IpfsSettings, secrets: &Secrets, ep: &Endpoints) -> Result<String> {
    let c = client()?;
    match s.mode {
        IpfsMode::Off => bail!("IPFS is turned off"),
        IpfsMode::Kubo => {
            let resp = with_kubo_auth(c.post(format!("{}/api/v0/version", kubo_base(s))), secrets)
                .send()
                .map_err(|_| anyhow!("no IPFS node answered at {}. Is IPFS Desktop or Kubo running?", kubo_base(s)))?;
            if !resp.status().is_success() {
                bail!("the IPFS node answered HTTP {}", resp.status().as_u16());
            }
            let v: serde_json::Value = resp.json().map_err(|_| anyhow!("that address did not answer like an IPFS node"))?;
            Ok(format!("Connected: IPFS {}", v.get("Version").and_then(|x| x.as_str()).unwrap_or("node")))
        }
        IpfsMode::Pinata => {
            let jwt = secrets.pinata_jwt.as_deref().filter(|j| !j.trim().is_empty()).ok_or_else(|| anyhow!("paste your Pinata JWT first"))?;
            let resp = c.get(&ep.pinata_auth).bearer_auth(jwt.trim()).send().map_err(|_| anyhow!("could not reach Pinata"))?;
            if !resp.status().is_success() {
                bail!("Pinata did not accept that key (HTTP {})", resp.status().as_u16());
            }
            Ok("Connected: Pinata key works".into())
        }
    }
}

/// Adds and pins one file. Returns its CID.
pub fn pin_file(s: &IpfsSettings, secrets: &Secrets, ep: &Endpoints, path: &Path, filename: &str) -> Result<Pinned> {
    let c = client()?;
    let bytes = std::fs::read(path)?;
    let part = || reqwest::blocking::multipart::Part::bytes(bytes.clone()).file_name(filename.to_string());
    match s.mode {
        IpfsMode::Off => bail!("IPFS is turned off"),
        IpfsMode::Kubo => {
            let form = reqwest::blocking::multipart::Form::new().part("file", part());
            let resp = with_kubo_auth(
                c.post(format!("{}/api/v0/add?pin=true&cid-version=1", kubo_base(s))).multipart(form),
                secrets,
            )
            .send()
            .map_err(|_| anyhow!("no IPFS node answered at {}", kubo_base(s)))?;
            if !resp.status().is_success() {
                bail!("the IPFS node refused the file (HTTP {})", resp.status().as_u16());
            }
            let v: serde_json::Value = resp.json()?;
            let cid = v.get("Hash").and_then(|h| h.as_str()).ok_or_else(|| anyhow!("the IPFS node returned no CID"))?;
            Ok(Pinned { cid: cid.to_string(), service: "kubo" })
        }
        IpfsMode::Pinata => {
            let jwt = secrets.pinata_jwt.as_deref().filter(|j| !j.trim().is_empty()).ok_or_else(|| anyhow!("no Pinata JWT set"))?;
            let form = reqwest::blocking::multipart::Form::new().text("network", "public").part("file", part());
            let resp = c.post(&ep.pinata_upload).bearer_auth(jwt.trim()).multipart(form).send().map_err(|_| anyhow!("could not reach Pinata"))?;
            if !resp.status().is_success() {
                bail!("Pinata refused the upload (HTTP {})", resp.status().as_u16());
            }
            let v: serde_json::Value = resp.json()?;
            let cid = v
                .pointer("/data/cid")
                .or_else(|| v.get("cid"))
                .and_then(|h| h.as_str())
                .ok_or_else(|| anyhow!("Pinata returned no CID"))?;
            Ok(Pinned { cid: cid.to_string(), service: "pinata" })
        }
    }
}

pub fn gateway_url(s: &IpfsSettings, cid: &str) -> String {
    format!("{}/ipfs/{}", s.gateway_or_default(), cid)
}
