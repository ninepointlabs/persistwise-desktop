//! Local feed polling: the job the Persistwise web app did on its servers,
//! now done on your computer. For each feed (RSS, Atom or JSON Feed):
//!
//! 1. fetch and parse it;
//! 2. for every entry not seen before, save a full local copy;
//! 3. retry earlier downloads that failed (up to `max_capture_attempts`);
//! 4. send pages without a snapshot to Save Page Now (if Wayback is on),
//!    one at a time, a few seconds apart, retrying failures about hourly;
//! 5. pin pages without a CID (if the user set up IPFS).
//!
//! Like the web app, the first check of a new feed archives every entry the
//! feed currently lists. A lock file stops two pollers (the open app and the
//! background timer) from working at the same time.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::time::Duration;
use url::Url;

use crate::capture::{self, CaptureOptions};
use crate::store::{Feed, NewPage, Page, Store};
use crate::wayback::Wayback;
use crate::{http, ipfs, settings, util};

#[derive(Debug, Clone)]
pub struct PollOptions {
    pub capture: CaptureOptions,
    /// None when Wayback backups are turned off.
    pub wayback: Option<Wayback>,
    /// Pin to IPFS (only when the user configured it).
    pub ipfs: bool,
    pub max_capture_attempts: i64,
    pub backup_retry_minutes: i64,
    /// Gap between Save Page Now submissions.
    pub wayback_spacing: Duration,
}

impl PollOptions {
    /// Options as the user set them in Settings.
    pub fn from_settings(store: &Store) -> Self {
        let s = settings::load(store.root());
        let secrets = settings::load_secrets(store.root());
        Self {
            capture: CaptureOptions::default(),
            wayback: s.wayback_on_save.then(Wayback::default),
            ipfs: s.ipfs.pin_on_save && ipfs::is_configured(&s.ipfs, &secrets),
            max_capture_attempts: 5,
            backup_retry_minutes: 55,
            wayback_spacing: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct FeedReport {
    pub feed_id: i64,
    pub feed: String,
    pub new_pages: usize,
    pub saved: usize,
    pub capture_failed: usize,
    pub wayback_done: usize,
    pub wayback_failed: usize,
    pub pinned: usize,
    pub pin_failed: usize,
    pub error: Option<String>,
}

/// Fetches and parses a feed. Relative entry links resolve against the feed URL.
pub fn fetch(url: &str) -> Result<feed_rs::model::Feed> {
    let client = http::client(Duration::from_secs(45))?;
    let resp = client
        .get(url)
        .send()
        .with_context(|| format!("could not reach {}", host(url)))?;
    if !resp.status().is_success() {
        bail!("the feed answered HTTP {}", resp.status().as_u16());
    }
    let bytes = http::read_limited(resp, 20 * 1024 * 1024)?;
    parse(&bytes, url)
}

pub fn parse(bytes: &[u8], url: &str) -> Result<feed_rs::model::Feed> {
    feed_rs::parser::Builder::new()
        .base_uri(Some(url))
        .id_generator(|links, title, uri| {
            // Entries without an id are keyed by their link, as the web app did.
            links
                .iter()
                .map(|l| l.href.clone())
                .find(|h| h.starts_with("http"))
                .unwrap_or_else(|| feed_rs::parser::generate_id(links, title, uri))
        })
        .build()
        .parse(bytes)
        .map_err(|e| anyhow!("that is not an RSS, Atom or JSON feed ({e})"))
}

/// Finds a feed advertised by an HTML page (`<link rel="alternate" type="application/rss+xml">`).
pub fn discover(html: &str, page_url: &Url) -> Option<String> {
    let re = regex::Regex::new(r#"(?is)<link\b[^>]*>"#).unwrap();
    let href_re = regex::Regex::new(r#"(?i)href\s*=\s*["']([^"']+)["']"#).unwrap();
    for m in re.find_iter(html) {
        let tag = m.as_str();
        let lower = tag.to_ascii_lowercase();
        if !lower.contains("alternate") {
            continue;
        }
        if !(lower.contains("application/rss+xml")
            || lower.contains("application/atom+xml")
            || lower.contains("application/feed+json"))
        {
            continue;
        }
        let href = href_re.captures(tag)?;
        let href = util::decode_entities(&href[1]);
        return page_url.join(href.trim()).ok().map(|u| u.to_string());
    }
    None
}

/// Subscribes to a feed. Accepts a feed URL or a web page that links to its
/// feed. Checks it parses before adding it. Returns the feed and whether it
/// was new.
pub fn subscribe(store: &Store, input: &str) -> Result<(Feed, bool)> {
    let url = Url::parse(input.trim()).context("that does not look like a web address")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only http and https feeds are supported");
    }
    let client = http::client(Duration::from_secs(45))?;
    let resp = client
        .get(url.as_str())
        .send()
        .with_context(|| format!("could not reach {}", host(url.as_str())))?;
    if !resp.status().is_success() {
        bail!("the site answered HTTP {}", resp.status().as_u16());
    }
    let final_url = resp.url().clone();
    let bytes = http::read_limited(resp, 20 * 1024 * 1024)?;
    let (feed_url, parsed) = match parse(&bytes, final_url.as_str()) {
        Ok(f) => (url.to_string(), f),
        Err(e) => {
            let html = util::decode_html(&bytes, None);
            let found = discover(&html, &final_url).ok_or(e)?;
            let f = fetch(&found)?;
            (found, f)
        }
    };
    let name = parsed
        .title
        .as_ref()
        .map(|t| t.content.trim().to_string())
        .filter(|t| !t.is_empty());
    let (id, created) = store.add_feed(&feed_url, name.as_deref(), Some("blog"))?;
    let feed = store.feed(id)?.ok_or_else(|| anyhow!("feed vanished"))?;
    Ok((feed, created))
}

struct EntryInfo {
    guid: String,
    url: String,
    title: Option<String>,
    author: Option<String>,
    published: Option<String>,
}

fn entry_info(e: &feed_rs::model::Entry) -> Option<EntryInfo> {
    let link = e
        .links
        .iter()
        .filter(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
        .map(|l| l.href.clone())
        .find(|h| h.starts_with("http://") || h.starts_with("https://"));
    let guid = Some(e.id.trim().to_string())
        .filter(|g| !g.is_empty())
        .or_else(|| link.clone())?;
    let url = link.or_else(|| {
        (guid.starts_with("http://") || guid.starts_with("https://")).then(|| guid.clone())
    })?;
    Some(EntryInfo {
        guid,
        url,
        title: e
            .title
            .as_ref()
            .map(|t| t.content.trim().to_string())
            .filter(|t| !t.is_empty()),
        author: e
            .authors
            .iter()
            .find_map(|a| a.name.as_deref().and_then(clean_author)),
        published: e
            .published
            .or(e.updated)
            .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    })
}

/// RSS authors often look like `me@example.com (My Name)`; keep the name.
fn clean_author(raw: &str) -> Option<String> {
    let mut a = raw.trim();
    if let Some(open) = a.find('(') {
        if a.ends_with(')') && (open == 0 || a[..open].contains('@')) {
            a = a[open + 1..a.len() - 1].trim();
        }
    }
    (!a.is_empty()).then(|| a.to_string())
}

/// Downloads a page into an existing row; records the failure otherwise.
fn capture_into(store: &Store, page: &Page, opts: &CaptureOptions) -> bool {
    match capture::capture(&page.url, opts) {
        Ok(cap) => {
            let ok = store
                .write_file(&page.uid, cap.ext, &cap.bytes)
                .and_then(|file| {
                    store.attach_capture(
                        page.id,
                        file,
                        &cap.content_type,
                        (cap.final_url != page.url).then_some(cap.final_url.as_str()),
                        cap.title.as_deref(),
                        &cap.text,
                        (cap.assets_inlined as i64, cap.assets_failed as i64),
                    )
                })
                .is_ok();
            if !ok {
                let _ = store.record_capture_failure(page.id, "could not write the saved copy");
            }
            ok
        }
        Err(e) => {
            let _ = store.record_capture_failure(page.id, &format!("{e:#}"));
            false
        }
    }
}

/// Checks one feed. `on_change` is called with each page as it changes, so
/// the UI can update live.
pub fn poll_feed(
    store: &Store,
    feed: &Feed,
    opts: &PollOptions,
    on_change: &mut dyn FnMut(i64),
) -> FeedReport {
    let mut report = FeedReport {
        feed_id: feed.id,
        feed: feed.name.clone().unwrap_or_else(|| feed.url.clone()),
        ..Default::default()
    };

    let parsed = match fetch(&feed.url) {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("{e:#}");
            let _ = store.set_feed_status(feed.id, "errored", Some(&msg), true);
            report.error = Some(msg);
            return report;
        }
    };
    if let Some(title) = parsed
        .title
        .as_ref()
        .map(|t| t.content.trim())
        .filter(|t| !t.is_empty())
    {
        let _ = store.set_feed_name(feed.id, title);
    }

    // New entries, oldest first.
    let mut tried = std::collections::HashSet::new();
    for entry in parsed.entries.iter().rev() {
        let Some(info) = entry_info(entry) else {
            continue;
        };
        if matches!(store.page_by_feed_guid(feed.id, &info.guid), Ok(Some(_))) {
            continue;
        }
        let uid = util::new_uid();
        let page = match store.insert_page(
            NewPage {
                url: info.url,
                title: info.title,
                author: info.author,
                feed_id: Some(feed.id),
                guid: Some(info.guid),
                published_at: info.published,
                content_type: "text/html".into(),
                origin: "feed".into(),
                ..Default::default()
            },
            None,
            &uid,
        ) {
            Ok(p) => p,
            Err(_) => continue, // another poller added it a moment ago
        };
        report.new_pages += 1;
        tried.insert(page.id);
        if capture_into(store, &page, &opts.capture) {
            report.saved += 1;
        } else {
            report.capture_failed += 1;
        }
        on_change(page.id);
    }

    // Earlier downloads that failed.
    for page in store
        .pages_needing_capture(feed.id, opts.max_capture_attempts)
        .unwrap_or_default()
    {
        if tried.contains(&page.id) {
            continue;
        }
        if capture_into(store, &page, &opts.capture) {
            report.saved += 1;
        } else {
            report.capture_failed += 1;
        }
        on_change(page.id);
    }

    // Wayback, one at a time and spaced out; stop early if Save Page Now is
    // rate-limiting so we do not hammer it.
    if let Some(wb) = &opts.wayback {
        let pending = store
            .pages_needing_wayback(feed.id, opts.backup_retry_minutes)
            .unwrap_or_default();
        for (i, page) in pending.iter().enumerate() {
            if i > 0 {
                std::thread::sleep(opts.wayback_spacing);
            }
            match crate::backup_wayback(store, page.id, wb) {
                Ok(p) if p.wayback_status == "done" => report.wayback_done += 1,
                Ok(p) => {
                    report.wayback_failed += 1;
                    on_change(page.id);
                    if p.wayback_error
                        .as_deref()
                        .is_some_and(|e| e.contains("rate-limiting"))
                    {
                        break;
                    }
                    continue;
                }
                Err(_) => report.wayback_failed += 1,
            }
            on_change(page.id);
        }
    }

    if opts.ipfs {
        for page in store
            .pages_needing_ipfs(feed.id, opts.backup_retry_minutes)
            .unwrap_or_default()
        {
            match crate::backup_ipfs(store, page.id) {
                Ok(p) if p.ipfs_status == "done" => report.pinned += 1,
                _ => report.pin_failed += 1,
            }
            on_change(page.id);
        }
    }

    let _ = store.set_feed_status(feed.id, "active", None, true);
    report
}

/// Raised when another poller (the app or the background timer) is busy.
#[derive(Debug)]
pub struct AlreadyPolling;

impl std::fmt::Display for AlreadyPolling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("feeds are already being checked")
    }
}
impl std::error::Error for AlreadyPolling {}

/// Checks every active or errored feed (paused ones are skipped), or just
/// `only` when given. Holds `poll.lock` in the data folder while working.
pub fn poll_all(
    store: &Store,
    only: Option<i64>,
    opts: &PollOptions,
    on_change: &mut dyn FnMut(i64),
) -> Result<Vec<FeedReport>> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store.root().join("poll.lock"))?;
    if lock.try_lock().is_err() {
        return Err(AlreadyPolling.into());
    }
    let feeds = store.feeds()?;
    let mut out = Vec::new();
    for feed in feeds {
        if only.is_some_and(|id| id != feed.id) {
            continue;
        }
        if only.is_none() && feed.status.as_deref() == Some("paused") {
            continue;
        }
        out.push(poll_feed(store, &feed, opts, on_change));
    }
    drop(lock);
    Ok(out)
}

fn host(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "the site".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_rss_authors() {
        assert_eq!(
            clean_author("a@b.c (A. Writer)").as_deref(),
            Some("A. Writer")
        );
        assert_eq!(clean_author("(A. Writer)").as_deref(), Some("A. Writer"));
        assert_eq!(
            clean_author("Jo (the editor)").as_deref(),
            Some("Jo (the editor)")
        );
        assert!(clean_author("  ").is_none());
    }

    #[test]
    fn discovers_feed_links() {
        let page = Url::parse("https://blog.example/post/1").unwrap();
        let html = r#"<head><link rel="stylesheet" href="/s.css"><link rel="alternate" type="application/rss+xml" title="RSS" href="/feed.xml?a=1&amp;b=2"></head>"#;
        assert_eq!(
            discover(html, &page).as_deref(),
            Some("https://blog.example/feed.xml?a=1&b=2")
        );
        assert!(discover("<link rel=icon href=x>", &page).is_none());
    }
}
