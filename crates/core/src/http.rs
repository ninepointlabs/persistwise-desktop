//! The one HTTP client the app uses. Plain requests to the pages you save and
//! the backup services you turn on; nothing else, no telemetry.

use anyhow::Result;
use std::time::Duration;

pub const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (compatible; PersistwiseDesktop/",
    env!("CARGO_PKG_VERSION"),
    "; +https://github.com/ninepointlabs/persistwise-desktop)"
);

pub fn client(timeout: Duration) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?)
}

/// A client that does not follow redirects (Save Page Now answers with one).
pub fn client_no_redirect(timeout: Duration) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// Reads a response body, giving up once it passes `max` bytes.
pub fn read_limited(resp: reqwest::blocking::Response, max: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    if let Some(len) = resp.content_length() {
        if len as usize > max {
            anyhow::bail!("too large ({len} bytes, limit {max})");
        }
    }
    let mut buf = Vec::new();
    resp.take(max as u64 + 1).read_to_end(&mut buf)?;
    if buf.len() > max {
        anyhow::bail!("too large (over {max} bytes)");
    }
    Ok(buf)
}
