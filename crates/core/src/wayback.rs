//! Optional backup to the Wayback Machine through Internet Archive's public
//! Save Page Now (https://web.archive.org/save). No account or keys: the
//! same thing anyone can do from the web form. Rate-limited by the Internet
//! Archive, so a failure here is normal and never blocks a local save.

use anyhow::{anyhow, bail, Result};
use regex::Regex;
use std::time::Duration;

pub const DEFAULT_BASE: &str = "https://web.archive.org";
/// Public availability API, used to link an existing snapshot when Save Page
/// Now declines a new capture (for example a URL's daily capture limit).
pub const DEFAULT_AVAILABILITY_BASE: &str = "https://archive.org";

#[derive(Debug, Clone)]
pub struct Wayback {
    pub base: String,
    pub availability_base: String,
    pub poll_interval: Duration,
    pub max_polls: u32,
    pub timeout: Duration,
}

impl Default for Wayback {
    fn default() -> Self {
        Self {
            base: DEFAULT_BASE.to_string(),
            availability_base: DEFAULT_AVAILABILITY_BASE.to_string(),
            poll_interval: Duration::from_secs(6),
            max_polls: 25,
            timeout: Duration::from_secs(90),
        }
    }
}

impl Wayback {
    /// Asks Save Page Now to capture `url` and waits for the result.
    /// Returns the snapshot URL (`https://web.archive.org/web/<ts>/<url>`).
    pub fn save(&self, url: &str) -> Result<String> {
        let client = crate::http::client_no_redirect(self.timeout)?;
        let resp = client
            .get(format!("{}/save/{}", self.base, url))
            .send()
            .map_err(|e| anyhow!("could not reach Save Page Now: {}", short(&e.to_string())))?;
        let status = resp.status();

        if let Some(snap) = header_snapshot(&resp, reqwest::header::LOCATION)
            .or_else(|| header_snapshot(&resp, reqwest::header::CONTENT_LOCATION))
        {
            return Ok(self.absolute(&snap));
        }
        let body = crate::http::read_limited(resp, 2 * 1024 * 1024).unwrap_or_default();
        let body = String::from_utf8_lossy(&body);

        if !status.is_success() {
            let msg = spn_message(&body);
            // Declined because the page was captured recently: link the
            // newest existing snapshot instead.
            if msg
                .as_deref()
                .is_some_and(|m| m.contains("already captured"))
            {
                if let Some(snap) = self.latest_snapshot(url) {
                    return Ok(snap);
                }
            }
            let detail = msg.map(|m| format!(": {m}")).unwrap_or_default();
            if status.as_u16() == 429 {
                bail!("Save Page Now is rate-limiting requests right now{detail}");
            }
            if status.is_server_error() {
                bail!(
                    "Save Page Now is having trouble (HTTP {}){detail}",
                    status.as_u16()
                );
            }
            bail!(
                "Save Page Now refused the capture (HTTP {}){detail}",
                status.as_u16()
            );
        }
        let job = job_id(&body).ok_or_else(|| {
            anyhow!(
                "Save Page Now did not start a capture{}",
                spn_message(&body)
                    .map(|m| format!(": {m}"))
                    .unwrap_or_default()
            )
        })?;
        self.wait_for(&client, &job, url)
    }

    /// Newest existing snapshot of `url`, from the public availability API.
    pub fn latest_snapshot(&self, url: &str) -> Option<String> {
        let client = crate::http::client(Duration::from_secs(30)).ok()?;
        let v: serde_json::Value = client
            .get(format!(
                "{}/wayback/available",
                self.availability_base.trim_end_matches('/')
            ))
            .query(&[("url", url)])
            .send()
            .ok()?
            .json()
            .ok()?;
        let closest = v.pointer("/archived_snapshots/closest")?;
        if closest.get("available").and_then(|a| a.as_bool()) == Some(false) {
            return None;
        }
        let snap = closest.get("url")?.as_str()?;
        Some(snap.replacen("http://web.archive.org", "https://web.archive.org", 1))
    }

    fn wait_for(&self, client: &reqwest::blocking::Client, job: &str, url: &str) -> Result<String> {
        for _ in 0..self.max_polls {
            std::thread::sleep(self.poll_interval);
            let resp = match client
                .get(format!("{}/save/status/{}", self.base, job))
                .send()
            {
                Ok(r) => r,
                Err(_) => continue,
            };
            if !resp.status().is_success() {
                continue;
            }
            let v: serde_json::Value = match resp.json() {
                Ok(v) => v,
                Err(_) => continue,
            };
            match v.get("status").and_then(|s| s.as_str()) {
                Some("success") => {
                    let ts = v
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .ok_or_else(|| anyhow!("Save Page Now gave no timestamp"))?;
                    let original = v
                        .get("original_url")
                        .and_then(|t| t.as_str())
                        .unwrap_or(url);
                    return Ok(format!("{}/web/{}/{}", self.public_base(), ts, original));
                }
                Some("pending") | None => continue,
                Some(_) => {
                    let msg = v
                        .get("message")
                        .and_then(|m| m.as_str())
                        .or_else(|| v.get("status_ext").and_then(|m| m.as_str()))
                        .unwrap_or("capture failed");
                    bail!("Save Page Now: {}", short(msg));
                }
            }
        }
        bail!("Save Page Now is still working on it; check again later")
    }

    fn public_base(&self) -> &str {
        self.base.trim_end_matches('/')
    }

    fn absolute(&self, path_or_url: &str) -> String {
        if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
            path_or_url.to_string()
        } else {
            format!("{}{}", self.public_base(), path_or_url)
        }
    }
}

fn header_snapshot(
    resp: &reqwest::blocking::Response,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    let v = resp.headers().get(name)?.to_str().ok()?;
    snapshot_re().is_match(v).then(|| v.to_string())
}

fn snapshot_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/web/\d{8,14}[a-z_]*/").unwrap())
}

/// Finds the capture job id in the Save Page Now HTML or JSON response.
pub fn job_id(body: &str) -> Option<String> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"(?:"job_id"\s*:\s*"|watchJob\(\s*["'])([A-Za-z0-9_\-]+)|\b(spn2-[0-9a-f]{16,64})\b"#,
        )
        .unwrap()
    });
    re.captures(body)
        .and_then(|c| c.get(1).or_else(|| c.get(2)))
        .map(|m| m.as_str().to_string())
}

/// The human-readable reason on a Save Page Now error page, if any.
fn spn_message(body: &str) -> Option<String> {
    static STRIP: std::sync::OnceLock<(Regex, Regex)> = std::sync::OnceLock::new();
    let (blocks, tags) = STRIP.get_or_init(|| {
        (
            Regex::new(r"(?is)<script.*?</script>|<style.*?</style>").unwrap(),
            Regex::new(r"<[^>]+>").unwrap(),
        )
    });
    let no_blocks = blocks.replace_all(body, " ");
    let text = tags.replace_all(&no_blocks, " ");
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(i) = text.find("Sorry") {
        let rest = &text[i + "Sorry".len()..];
        let end = ["Please email", "Return to Save Page Now"]
            .iter()
            .filter_map(|m| rest.find(m))
            .min()
            .unwrap_or(rest.len());
        let m = rest[..end].trim();
        if !m.is_empty() {
            return Some(short(m));
        }
    }
    static P: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = P.get_or_init(|| Regex::new(r#"(?s)<p>([^<]{10,300})</p>"#).unwrap());
    re.captures(body).map(|c| short(c[1].trim()))
}

fn short(s: &str) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 200 {
        format!("{}…", s.chars().take(200).collect::<String>())
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_job_ids() {
        assert_eq!(
            job_id(r#"{"url":"x","job_id":"spn2-abc123def4567890"}"#).as_deref(),
            Some("spn2-abc123def4567890")
        );
        assert_eq!(
            job_id(
                r#"<script>spn.watchJob("spn2-0123456789abcdef0123", "/_static/",6000);</script>"#
            )
            .as_deref(),
            Some("spn2-0123456789abcdef0123")
        );
        assert!(job_id("<html>nothing</html>").is_none());
    }

    #[test]
    fn reads_the_reason_from_an_error_page() {
        let page = r#"<html><script>var x="Sorry no";</script><h2>Sorry</h2>
            <p>This URL has been already captured 5 times today, which is a daily limit we have set for that Resource type. Please try again tomorrow.
            Please email us at "info@archive.org"</p><a>Return to Save Page Now</a></html>"#;
        let m = spn_message(page).unwrap();
        assert!(
            m.starts_with("This URL has been already captured 5 times today"),
            "{m}"
        );
        assert!(!m.contains("info@archive.org"));
    }
}
