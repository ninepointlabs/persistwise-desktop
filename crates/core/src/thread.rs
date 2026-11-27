//! Saves a whole multi-page thread (a forum topic, a paginated blog archive)
//! as one grouped item.
//!
//! Starting from the address the user gave, each page is fetched once, saved
//! exactly like a single page (same capture, same size caps), and then its
//! "next page" is looked for, in this order:
//!
//! 1. an HTTP `Link: <…>; rel="next"` header,
//! 2. `<link rel="next">` in the page,
//! 3. `<a rel="next">`,
//! 4. a link marked as "next" by its text, title, aria-label or class
//!    (or a parent's `next` class), that stays in the same thread,
//! 5. the same thread address with the next page number or offset in a
//!    common pagination form (`?page=2`, `&start=15`, `/page-2`, `/page/2`,
//!    `/page2`, …).
//!
//! Only links on the same host are followed (http and https count as the
//! same site; on an https page, a same-site `http://` next link is fetched
//! over https). Two addresses are the same page when they differ only in
//! http/https, a leading `www.`, the fragment, or a session id in the query
//! (see [`page_identity`]). The walk stops when no next page
//! is found, when a page leads back to one already seen, or at
//! [`ThreadOptions::max_pages`] (then the thread is marked partial). A failed
//! page also ends the walk; the pages saved before it are kept.
//!
//! Saving a thread that is already archived (from any of its page addresses)
//! continues it: the earlier pages are reused without fetching them, the
//! last saved page is fetched again and its copy replaced if it changed (new
//! replies, a new "next" link), and any pages after it are appended to the
//! same thread under the same rules. When the last page is replaced, the
//! IPFS pin of its earlier copy is removed (unless another saved page has
//! the same CID); the new copy is pinned like any new save.

use anyhow::{bail, Result};
use lol_html::html_content::{Element, TextChunk};
use lol_html::{element, text};
use regex::Regex;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;
use url::Url;

use crate::capture::{self, CaptureOptions, Fetched};
use crate::store::{NewPage, Page, Store, Thread};
use crate::{http, text as textx, util};

/// The most pages one thread save will follow.
pub const MAX_THREAD_PAGES: usize = 100;

#[derive(Debug, Clone)]
pub struct ThreadOptions {
    pub max_pages: usize,
    /// Pause between page fetches, to be polite to the site.
    pub delay: Duration,
    pub capture: CaptureOptions,
    /// Where an outdated IPFS pin is removed (see [`crate::release_pin`]).
    pub ipfs: crate::ipfs::Endpoints,
}

impl Default for ThreadOptions {
    fn default() -> Self {
        Self {
            max_pages: MAX_THREAD_PAGES,
            delay: Duration::from_millis(1500),
            capture: CaptureOptions::default(),
            ipfs: crate::ipfs::Endpoints::default(),
        }
    }
}

/// Reported before each page is fetched.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadProgress {
    /// 1-based.
    pub page: usize,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadSave {
    /// The grouped item. None when the address turned out to be a single page.
    pub thread: Option<Thread>,
    /// The pages of this save, in order.
    pub pages: Vec<Page>,
    /// Pages captured now (the others were already in the archive).
    pub new_page_ids: Vec<i64>,
    /// Pages whose saved copy was replaced by a newer one (on a re-save, the
    /// thread's last page when it had changed).
    pub refreshed_page_ids: Vec<i64>,
    /// How many pages were already in the archive and reused as they were.
    pub reused: usize,
    /// IPFS CIDs of replaced copies whose pins were removed.
    pub unpinned: Vec<String>,
    /// True when this save continued a thread that was already archived.
    pub resumed: bool,
    /// True when no further pages were found.
    pub single: bool,
    /// True when the save stopped early (page cap or a failed page).
    pub partial: bool,
    /// A sentence for the user.
    pub message: String,
}

/// Saves `url` and every following page of its thread. If `url` is a page
/// of a thread that is already archived, that thread is continued instead
/// (see the module docs).
pub fn save_thread(
    store: &Store,
    url: &str,
    opts: &ThreadOptions,
    on_progress: &mut dyn FnMut(ThreadProgress),
) -> Result<ThreadSave> {
    let typed = url.trim().to_string();
    let typed_url =
        Url::parse(&typed).map_err(|_| anyhow::anyhow!("that does not look like a web address"))?;
    if !matches!(typed_url.scheme(), "http" | "https") {
        bail!("only http and https pages can be saved");
    }
    let client = http::client(opts.capture.timeout)?;
    let max = opts.max_pages.max(1);

    // An archived thread that this address belongs to.
    let existing = match find_page(store, &[typed.as_str()])? {
        Some(p) => match store.thread_of_page(p.id)? {
            Some((tid, _)) => store.thread(tid)?.filter(|t| !t.pages.is_empty()),
            None => None,
        },
        None => None,
    };

    let mut seen: HashSet<String> = HashSet::new();
    let mut pages: Vec<Page> = Vec::new();
    let mut new_ids = Vec::new();
    let mut refreshed = Vec::new();
    // (cid, service) of copies replaced by a refresh.
    let mut old_pins: Vec<(String, String)> = Vec::new();
    let mut reused = 0;
    // Why the walk stopped early, if it did.
    let mut stopped: Option<String> = None;
    let resumed = existing.is_some();
    let thread_id = existing.as_ref().map(|t| t.id);

    let (start, mut next) = match existing {
        Some(t) => {
            // Same-site checks stay anchored on where the thread began.
            let start = Url::parse(&t.url).unwrap_or_else(|_| typed_url.clone());
            let mut earlier = t.pages;
            let last = earlier.pop().expect("thread has pages");
            for p in &earlier {
                for u in std::iter::once(&p.url).chain(p.final_url.as_ref()) {
                    if let Ok(u) = Url::parse(u) {
                        seen.insert(norm(&u));
                    }
                }
            }
            reused = earlier.len();
            pages = earlier;

            // Refresh the last saved page: new replies, maybe a new next link.
            let last_url = Url::parse(&last.url)
                .map_err(|_| anyhow::anyhow!("the thread's last page has a bad address"))?;
            seen.insert(norm(&last_url));
            let n = pages.len() + 1;
            on_progress(ThreadProgress {
                page: n,
                url: last_url.to_string(),
            });
            let fetched = capture::fetch_document(last_url.as_str(), &client, &opts.capture)
                .map_err(|e| {
                    e.context(format!(
                        "could not check page {n} ({last_url}) for new replies; the saved thread is unchanged"
                    ))
                })?;
            seen.insert(norm(&fetched.final_url));
            let following = next_page(&fetched, &start);
            let page = match refresh_fetched(store, &last, fetched, client.clone(), &opts.capture)
                .map_err(|e| {
                e.context(format!(
                    "could not update page {n} ({last_url}); the saved thread is unchanged"
                ))
            })? {
                Some(p) => {
                    refreshed.push(p.id);
                    if let (Some(cid), Some(service)) = (&last.cid, &last.pin_service) {
                        old_pins.push((cid.clone(), service.clone()));
                    }
                    p
                }
                None => {
                    reused += 1;
                    last
                }
            };
            pages.push(page);
            (start, following)
        }
        None => (typed_url.clone(), Some(typed_url.clone())),
    };

    while let Some(cur) = next.take() {
        if pages.len() >= max {
            stopped = Some(format!(
                "stopped at the {max}-page limit; later pages were not saved"
            ));
            break;
        }
        if !seen.insert(norm(&cur)) {
            break;
        }
        if !pages.is_empty() && !opts.delay.is_zero() {
            std::thread::sleep(opts.delay);
        }
        let n = pages.len() + 1;
        on_progress(ThreadProgress {
            page: n,
            url: cur.to_string(),
        });
        let fetched = match capture::fetch_document(cur.as_str(), &client, &opts.capture) {
            Ok(f) => f,
            Err(e) if pages.is_empty() => return Err(e),
            Err(e) => {
                stopped = Some(format!("page {n} ({cur}) could not be saved: {e:#}"));
                break;
            }
        };
        // A "next" page that redirects back to one we have is the end.
        if n > 1 && fetched.final_url != cur && !seen.insert(norm(&fetched.final_url)) {
            break;
        }
        let following = next_page(&fetched, &start);

        let as_typed = if n == 1 { typed.as_str() } else { cur.as_str() };
        let existing = find_page(store, &[as_typed, cur.as_str(), fetched.final_url.as_str()])?;
        let page = match existing {
            Some(p) => {
                reused += 1;
                p
            }
            None => match save_fetched(store, as_typed, fetched, client.clone(), &opts.capture) {
                Ok(p) => {
                    new_ids.push(p.id);
                    p
                }
                Err(e) if pages.is_empty() => return Err(e),
                Err(e) => {
                    stopped = Some(format!("page {n} ({cur}) could not be saved: {e:#}"));
                    break;
                }
            },
        };
        pages.push(page);
        next = following;
    }

    let single = !resumed && pages.len() == 1 && stopped.is_none();
    if single {
        let message = if reused == 1 {
            "No further pages found, so this did not look like a multi-page thread. That page was already in your archive.".to_string()
        } else {
            "Saved 1 page. No further pages were found, so this did not look like a multi-page thread.".to_string()
        };
        return Ok(ThreadSave {
            thread: None,
            pages,
            new_page_ids: new_ids,
            refreshed_page_ids: refreshed,
            reused,
            unpinned: Vec::new(),
            resumed: false,
            single: true,
            partial: false,
            message,
        });
    }

    let partial = stopped.is_some();
    let status = if partial { "partial" } else { "complete" };
    // Position of pages[0] in the thread.
    let (thread_id, base) = match thread_id {
        Some(id) => {
            store.update_thread(id, status, stopped.as_deref())?;
            (id, 0)
        }
        None => {
            // A new save that starts on a page of an existing thread (for
            // example one reached through a redirect) continues that thread
            // instead of starting a second one.
            match store.thread_of_page(pages[0].id)? {
                Some((id, pos)) => {
                    store.update_thread(id, status, stopped.as_deref())?;
                    (id, pos)
                }
                None => (
                    store.create_thread(
                        start.as_str(),
                        pages[0].title.as_deref(),
                        status,
                        stopped.as_deref(),
                    )?,
                    0,
                ),
            }
        }
    };
    for (i, p) in pages.iter().enumerate() {
        store.set_thread_page(thread_id, p.id, base + i as i64)?;
    }
    let thread = store.thread(thread_id)?;
    let pages: Vec<Page> = pages
        .iter()
        .filter_map(|p| store.page(p.id).ok().flatten())
        .collect();
    let count = pages.len();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut message = if resumed {
        let added = new_ids.len();
        let last_note = if refreshed.is_empty() {
            "the last saved page had not changed"
        } else {
            "the last saved page was updated"
        };
        if added == 0 {
            format!("Checked the thread for new pages: none found, and {last_note}. It has {count} page{}", plural(count))
        } else {
            format!(
                "Updated the thread: {added} new page{} added and {last_note}. It now has {count} page{}",
                plural(added),
                plural(count)
            )
        }
    } else {
        let mut m = format!("Saved {count} page{} of the thread", plural(count));
        if reused > 0 {
            m.push_str(&format!(" ({reused} already in your archive)"));
        }
        m
    };
    match &stopped {
        Some(why) => message.push_str(&format!(". This is a partial thread: {why}.")),
        None => message.push('.'),
    }
    // The replaced copy's pin no longer matches anything in the archive.
    let mut unpinned = Vec::new();
    for (cid, service) in old_pins {
        match crate::release_pin(store, &cid, &service, &opts.ipfs) {
            Ok(true) => {
                message.push_str(" The IPFS pin of that page's earlier copy was removed.");
                unpinned.push(cid);
            }
            Ok(false) => {}
            Err(e) => message.push_str(&format!(
                " The IPFS pin of that page's earlier copy ({cid}) could not be removed: {e:#}. You can remove it by hand."
            )),
        }
    }
    Ok(ThreadSave {
        thread,
        pages,
        new_page_ids: new_ids,
        refreshed_page_ids: refreshed,
        reused,
        unpinned,
        resumed,
        single: false,
        partial,
        message,
    })
}

/// Replaces `page`'s copy with `fetched` if the content changed. Returns the
/// updated page, or None when the new copy is identical.
fn refresh_fetched(
    store: &Store,
    page: &Page,
    fetched: Fetched,
    client: reqwest::blocking::Client,
    opts: &CaptureOptions,
) -> Result<Option<Page>> {
    let cap = capture::capture_fetched(fetched, client, opts)?;
    if page.sha256.as_deref() == Some(util::sha256_hex(&cap.bytes).as_str()) {
        return Ok(None);
    }
    // Every HTML copy carries the time it was saved; that alone is no change.
    let old = store.file_path(page).and_then(|p| std::fs::read(p).ok());
    if old.is_some_and(|old| without_saved_at(&old) == without_saved_at(&cap.bytes)) {
        return Ok(None);
    }
    let file = store.write_file(&page.uid, cap.ext, &cap.bytes)?;
    let final_url = (cap.final_url != page.url).then_some(cap.final_url.as_str());
    store.refresh_page(
        page.id,
        file,
        &cap.content_type,
        final_url,
        cap.title.as_deref(),
        &cap.text,
        (cap.assets_inlined as i64, cap.assets_failed as i64),
    )?;
    store.page(page.id)
}

/// A saved copy with its `persistwise:saved-at` stamp taken out.
fn without_saved_at(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    static RE: OnceLock<regex::bytes::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::bytes::Regex::new(r#"<meta name="persistwise:saved-at" content="[^"]*">"#).unwrap()
    })
    .replace(bytes, &b""[..])
}

fn save_fetched(
    store: &Store,
    url: &str,
    fetched: Fetched,
    client: reqwest::blocking::Client,
    opts: &CaptureOptions,
) -> Result<Page> {
    let cap = capture::capture_fetched(fetched, client, opts)?;
    let uid = util::new_uid();
    let file = store.write_file(&uid, cap.ext, &cap.bytes)?;
    store.insert_page(
        NewPage {
            url: url.to_string(),
            final_url: (cap.final_url != url).then(|| cap.final_url.clone()),
            title: cap.title,
            content_type: cap.content_type,
            body_text: cap.text,
            assets_inlined: cap.assets_inlined as i64,
            assets_failed: cap.assets_failed as i64,
            origin: "desktop".into(),
            ..Default::default()
        },
        Some(file),
        &uid,
    )
}

/// For "have we been here" checks: see [`page_identity`].
fn norm(u: &Url) -> String {
    page_identity(u)
}

/// The saved page for any of `urls`: first an exact address match, then a
/// saved page whose address is the same page by [`page_identity`] (another
/// session id, http instead of https, …).
fn find_page(store: &Store, urls: &[&str]) -> Result<Option<Page>> {
    if let Some(p) = store.page_by_url(urls)? {
        return Ok(Some(p));
    }
    for u in urls {
        let Ok(parsed) = Url::parse(u) else { continue };
        let Some((host, _)) = host_key(&parsed) else {
            continue;
        };
        let want = page_identity(&parsed);
        let same = |s: &str| Url::parse(s).is_ok_and(|s| page_identity(&s) == want);
        if let Some(p) = store.page_by_url_matching(&host, &same)? {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

/// Host (lowercase, without a leading `www.`) and port. The port is None
/// when it is the scheme's default, so `http://x/` and `https://x/` share a
/// key while an explicit non-default port stays distinct.
fn host_key(u: &Url) -> Option<(String, Option<u16>)> {
    let h = u.host_str()?.to_ascii_lowercase();
    let h = h.strip_prefix("www.").unwrap_or(&h).to_string();
    Some((h, u.port()))
}

/// A query parameter that only carries a visitor's session, not content:
/// `PHPSESSID` (any value), and `s` / `sid` (Invision Power Board, phpBB)
/// when empty or a hex token of 16 to 64 characters. A short or non-hex `s`
/// or `sid` (a WordPress search, a numeric story id) is kept.
fn is_session_param(k: &str, v: &str) -> bool {
    if k.eq_ignore_ascii_case("phpsessid") {
        return true;
    }
    (k.eq_ignore_ascii_case("s") || k.eq_ignore_ascii_case("sid"))
        && (v.is_empty()
            || (16..=64).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The query pairs of `u` without session parameters.
fn content_query(u: &Url) -> Vec<(String, String)> {
    u.query_pairs()
        .filter(|(k, v)| !is_session_param(k, v))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// Which page an address names, for loop detection and finding a page in
/// the archive: host and non-default port as in [`host_key`] (so http and
/// https, and a leading `www.`, do not matter), path, and the query without
/// session parameters. The fragment is ignored.
pub fn page_identity(u: &Url) -> String {
    let (h, port) = host_key(u).unwrap_or_default();
    let port = port.map(|p| format!(":{p}")).unwrap_or_default();
    let q: Vec<String> = content_query(u)
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    let q = if q.is_empty() {
        String::new()
    } else {
        format!("?{}", q.join("&"))
    };
    format!("{h}{port}{}{q}", u.path())
}

/// Same site: same host (ignoring a leading `www.`) and port. http and https
/// count as the same site.
pub fn same_host(a: &Url, b: &Url) -> bool {
    matches!(b.scheme(), "http" | "https") && host_key(a).is_some() && host_key(a) == host_key(b)
}

/// One `<a href>` in the page and what might mark it as "next".
#[derive(Default, Debug)]
struct Anchor {
    href: String,
    rel: String,
    class: String,
    label: String,
    text: String,
    in_next_container: bool,
}

/// Finds the next page of the thread `fetched` belongs to. `start` is the
/// first page (only links on its host are followed).
pub fn next_page(fetched: &Fetched, start: &Url) -> Option<Url> {
    let here = &fetched.final_url;
    let ok = |u: &Url| same_host(start, u) && norm(u) != norm(here);
    // Old forums often link `http://` from their own https pages; do not
    // drop to http for the rest of the walk.
    let upgrade = |mut u: Url| -> Url {
        if here.scheme() == "https" && u.scheme() == "http" && same_host(here, &u) {
            let _ = u.set_scheme("https");
        }
        u
    };

    for h in &fetched.link_headers {
        for target in link_header_next(h) {
            if let Ok(u) = here.join(&target) {
                if ok(&u) {
                    return Some(upgrade(u));
                }
            }
        }
    }
    if !fetched.is_html() {
        return None;
    }
    let html = fetched.html();
    let link_next: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let base_href: RefCell<Option<String>> = RefCell::new(None);
    let anchors: RefCell<Vec<Anchor>> = RefCell::new(Vec::new());
    let get = |el: &Element, n: &str| {
        el.get_attribute(n)
            .map(|v| util::decode_entities(&v))
            .unwrap_or_default()
    };
    let _ = lol_html::rewrite_str(
        &html,
        textx::lenient()
            .append_element_content_handler(element!("base[href]", |el: &mut Element| {
                let mut b = base_href.borrow_mut();
                if b.is_none() {
                    *b = Some(get(el, "href"));
                }
                Ok(())
            }))
            .append_element_content_handler(element!("link[rel][href]", |el: &mut Element| {
                if has_token(&get(el, "rel"), "next") {
                    link_next.borrow_mut().push(get(el, "href"));
                }
                Ok(())
            }))
            .append_element_content_handler(element!("a[href]", |el: &mut Element| {
                anchors.borrow_mut().push(Anchor {
                    href: get(el, "href"),
                    rel: get(el, "rel"),
                    class: get(el, "class"),
                    label: format!("{} {}", get(el, "aria-label"), get(el, "title")),
                    ..Default::default()
                });
                Ok(())
            }))
            .append_element_content_handler(element!(
                ".next a[href], .pagination-next a[href], .pager-next a[href], .pager__item--next a[href], .page-next a[href]",
                |_el: &mut Element| {
                    if let Some(a) = anchors.borrow_mut().last_mut() {
                        a.in_next_container = true;
                    }
                    Ok(())
                }
            ))
            .append_element_content_handler(text!("a[href]", |t: &mut TextChunk| {
                if let Some(a) = anchors.borrow_mut().last_mut() {
                    a.text.push_str(t.as_str());
                }
                Ok(())
            })),
    );
    let base = base_href
        .into_inner()
        .and_then(|b| here.join(b.trim()).ok())
        .unwrap_or_else(|| here.clone());
    let resolve = |href: &str| -> Option<Url> {
        let v = href.trim();
        if v.is_empty() || v.starts_with('#') || v.to_ascii_lowercase().starts_with("javascript:") {
            return None;
        }
        base.join(v).ok().filter(|u| ok(u)).map(upgrade)
    };

    // 2. <link rel="next">
    if let Some(u) = link_next.borrow().iter().find_map(|h| resolve(h)) {
        return Some(u);
    }
    let anchors = anchors.into_inner();
    // 3. <a rel="next">
    if let Some(u) = anchors
        .iter()
        .filter(|a| has_token(&a.rel, "next"))
        .find_map(|a| resolve(&a.href))
    {
        return Some(u);
    }
    let here_key = page_key(here);
    // 4. A link that says "next" and stays in this thread.
    if let Some(u) = anchors
        .iter()
        .filter(|a| looks_like_next(a))
        .filter_map(|a| resolve(&a.href))
        .find(|u| {
            let k = page_key(u);
            k.base == here_key.base && k.number.is_some()
                || (u.path() == here.path() && u.query() != here.query())
        })
    {
        return Some(u);
    }
    // 5. The next page number in a recognised pagination form.
    let current = here_key.number.unwrap_or(match here_key.kind {
        PageKind::Offset => 0,
        PageKind::Number => 1,
    });
    anchors
        .iter()
        .filter_map(|a| resolve(&a.href))
        .filter_map(|u| {
            let k = page_key(&u);
            match k.number {
                Some(n) if k.base == here_key.base && n > current => Some((n, u)),
                _ => None,
            }
        })
        .min_by_key(|(n, _)| *n)
        .map(|(_, u)| u)
}

fn has_token(list: &str, want: &str) -> bool {
    list.split_ascii_whitespace()
        .any(|t| t.eq_ignore_ascii_case(want))
}

fn looks_like_next(a: &Anchor) -> bool {
    if a.in_next_container {
        return true;
    }
    let class_next = a.class.split_ascii_whitespace().any(|c| {
        let c = c.to_ascii_lowercase();
        c == "next"
            || c == "nextpage"
            || c == "next_page"
            || c.starts_with("next-")
            || c.ends_with("-next")
            || c.ends_with("_next")
            || c.ends_with("--next")
    });
    if class_next {
        return true;
    }
    let label = a.label.trim().to_lowercase();
    if label == "next" || label.contains("next page") {
        return true;
    }
    let text: String = a.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = text.to_lowercase();
    if matches!(text.as_str(), "›" | ">" | "→" | "next" | "next page") {
        return true;
    }
    let core = text
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string();
    core == "next" || core == "next page"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageKind {
    /// page=2, /page-2: counts pages, the first page is 1.
    Number,
    /// start=15, offset=30: counts items, the first page is 0.
    Offset,
}

#[derive(Debug, PartialEq)]
struct PageKey {
    /// The URL with its page number and session parameters taken out (and
    /// no scheme, default port or fragment).
    base: String,
    number: Option<u64>,
    kind: PageKind,
}

const NUMBER_PARAMS: &[&str] = &[
    "page",
    "p",
    "pg",
    "pagenum",
    "pageno",
    "page_no",
    "paged",
    "currentpage",
];
const OFFSET_PARAMS: &[&str] = &["start", "offset", "from", "st"];

fn path_page_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(?:page|p|pg)[-_]?(\d{1,6})(?:\.html?)?$").unwrap())
}

/// Splits a URL into "which thread" and "which page of it".
fn page_key(u: &Url) -> PageKey {
    let mut number = None;
    let mut kind = PageKind::Number;
    let mut query: Vec<(String, String)> = Vec::new();
    for (k, v) in content_query(u) {
        let lk = k.to_ascii_lowercase();
        let n = v.parse::<u64>().ok();
        if number.is_none() && n.is_some() && NUMBER_PARAMS.contains(&lk.as_str()) {
            number = n;
        } else if number.is_none() && n.is_some() && OFFSET_PARAMS.contains(&lk.as_str()) {
            number = n;
            kind = PageKind::Offset;
        } else {
            query.push((k, v));
        }
    }
    query.sort();
    let mut segs: Vec<String> = u
        .path_segments()
        .map(|s| s.map(str::to_string).collect())
        .unwrap_or_default();
    while segs.last().is_some_and(|s| s.is_empty()) {
        segs.pop();
    }
    if number.is_none() {
        if let Some(last) = segs.last() {
            if let Some(c) = path_page_re().captures(last) {
                number = c[1].parse().ok();
                segs.pop();
            } else if segs.len() >= 2
                && segs[segs.len() - 2].eq_ignore_ascii_case("page")
                && last.chars().all(|c| c.is_ascii_digit())
                && !last.is_empty()
            {
                number = last.parse().ok();
                segs.truncate(segs.len() - 2);
            }
        }
    }
    let q: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
    PageKey {
        base: format!(
            "{}/{}?{}",
            host_key(u)
                .map(|(h, p)| format!("{h}:{}", p.unwrap_or(0)))
                .unwrap_or_default(),
            segs.join("/"),
            q.join("&")
        ),
        number,
        kind,
    }
}

/// Targets of `rel="next"` entries in one `Link:` header value.
fn link_header_next(value: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"<([^>]*)>((?:\s*;\s*[^;,<]+)*)").unwrap());
    re.captures_iter(value)
        .filter(|c| {
            c[2].split(';').any(|param| {
                let Some((k, v)) = param.split_once('=') else {
                    return false;
                };
                k.trim().eq_ignore_ascii_case("rel")
                    && has_token(v.trim().trim_matches('"'), "next")
            })
        })
        .map(|c| c[1].trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetched(url: &str, html: &str) -> Fetched {
        Fetched {
            final_url: Url::parse(url).unwrap(),
            header_ct: Some("text/html".into()),
            mime: "text/html".into(),
            bytes: html.as_bytes().to_vec(),
            link_headers: vec![],
        }
    }

    fn next_of(url: &str, html: &str) -> Option<String> {
        let f = fetched(url, html);
        next_page(&f, &f.final_url).map(|u| u.to_string())
    }

    #[test]
    fn link_headers() {
        assert_eq!(
            link_header_next(
                r#"<https://x.test/a?page=2>; rel="next", <https://x.test/a?page=9>; rel="last""#
            ),
            vec!["https://x.test/a?page=2"]
        );
        assert_eq!(link_header_next(r#"</t/2>; rel=next"#), vec!["/t/2"]);
        assert!(
            link_header_next(r#"<https://x.test/style.css>; rel=preload; as=style"#).is_empty()
        );
    }

    #[test]
    fn phpbb_start_offsets() {
        let html = r#"<div class="pagination"><a href="./viewtopic.php?f=2&amp;t=77&amp;start=15">2</a>
            <a href="./viewtopic.php?f=2&amp;t=77&amp;start=30">3</a>
            <a href="./viewtopic.php?f=2&amp;t=78">Next topic</a></div>"#;
        assert_eq!(
            next_of("https://forum.test/viewtopic.php?f=2&t=77", html).as_deref(),
            Some("https://forum.test/viewtopic.php?f=2&t=77&start=15")
        );
        assert_eq!(
            next_of("https://forum.test/viewtopic.php?f=2&t=77&start=15", html).as_deref(),
            Some("https://forum.test/viewtopic.php?f=2&t=77&start=30")
        );
        assert_eq!(
            next_of("https://forum.test/viewtopic.php?f=2&t=77&start=30", html),
            None
        );
    }

    #[test]
    fn xenforo_and_vbulletin_paths() {
        let html = r#"<nav><a href="/threads/old-cars.123/">1</a><a href="/threads/old-cars.123/page-2">2</a>
            <a href="/threads/old-cars.123/page-3">3</a><a href="/threads/other.124/page-2">x</a></nav>"#;
        assert_eq!(
            next_of("https://xf.test/threads/old-cars.123/", html).as_deref(),
            Some("https://xf.test/threads/old-cars.123/page-2")
        );
        assert_eq!(
            next_of("https://xf.test/threads/old-cars.123/page-2", html).as_deref(),
            Some("https://xf.test/threads/old-cars.123/page-3")
        );
        let vb = r#"<a href="showthread.php?t=55&amp;page=2">2</a><a href="showthread.php?t=56">other</a>"#;
        assert_eq!(
            next_of("https://vb.test/showthread.php?t=55", vb).as_deref(),
            Some("https://vb.test/showthread.php?t=55&page=2")
        );
        let blog = r#"<a href="/page/2/">Older</a>"#;
        assert_eq!(
            next_of("https://blog.test/", blog).as_deref(),
            Some("https://blog.test/page/2/")
        );
    }

    #[test]
    fn page_one_link_is_not_next() {
        let html = r#"<a href="/t/topic/9?page=1">1</a><a href="/t/topic/9?page=2">2</a>"#;
        assert_eq!(
            next_of("https://d.test/t/topic/9", html).as_deref(),
            Some("https://d.test/t/topic/9?page=2")
        );
    }

    #[test]
    fn next_text_and_classes() {
        let html = r#"<a href="/forum/t-5?sort=new">Next ›</a>"#;
        assert_eq!(
            next_of("https://f.test/forum/t-5", html).as_deref(),
            Some("https://f.test/forum/t-5?sort=new")
        );
        let html = r#"<ul><li class="arrow next"><a class="button" href="/topic/9/?pg=2"><span>x</span></a></li></ul>"#;
        assert_eq!(
            next_of("https://f.test/topic/9/", html).as_deref(),
            Some("https://f.test/topic/9/?pg=2")
        );
        // A "next" link into another topic is not this thread's next page.
        let html = r#"<a href="/topic/10/">Next</a>"#;
        assert_eq!(next_of("https://f.test/topic/9/", html), None);
    }

    #[test]
    fn rel_next_wins_and_must_stay_on_site() {
        let html =
            r#"<link rel="next" href="https://elsewhere.test/p2"><a rel="next" href="/p2">n</a>"#;
        assert_eq!(
            next_of("https://www.site.test/p1", html).as_deref(),
            Some("https://www.site.test/p2")
        );
        let html = r#"<link rel="next" href="https://elsewhere.test/p2">"#;
        assert_eq!(next_of("https://site.test/p1", html), None);
    }
    /// Invision Power Board numbered page links (no rel="next"), `&st=`
    /// offsets with a 30-post step and a session id on every link.
    const IPB: &str = r#"<div class="pagelinks">
        <a href="http://forum.test/index.php?s=0123456789abcdef0123456789abcdef&amp;showtopic=123&amp;st=0">1</a>
        <a href="http://forum.test/index.php?s=0123456789abcdef0123456789abcdef&amp;showtopic=123&amp;st=30">2</a>
        <a href="http://forum.test/index.php?s=0123456789abcdef0123456789abcdef&amp;showtopic=123&amp;st=60">3</a>
        <a href="http://forum.test/index.php?s=0123456789abcdef0123456789abcdef&amp;showtopic=123&amp;st=90">4</a>
        <a href="http://forum.test/index.php?s=0123456789abcdef0123456789abcdef&amp;showtopic=124">Other topic</a>
        <a href="http://forum.test/index.php?s=0123456789abcdef0123456789abcdef&amp;showforum=7&amp;st=30">Forum page 2</a>
        </div>"#;

    #[test]
    fn ipb_st_offsets_with_http_links_and_session_ids() {
        let s = "s=0123456789abcdef0123456789abcdef&";
        let want = |st: u32| format!("https://forum.test/index.php?{s}showtopic=123&st={st}");
        // From the address as typed: https, no session id.
        assert_eq!(
            next_of("https://forum.test/index.php?showtopic=123", IPB),
            Some(want(30))
        );
        // From a later page reached with a different session id.
        assert_eq!(
            next_of(
                "https://forum.test/index.php?s=fedcba9876543210fedcba9876543210&showtopic=123&st=30",
                IPB
            ),
            Some(want(60))
        );
        assert_eq!(
            next_of("https://forum.test/index.php?showtopic=123&st=60", IPB),
            Some(want(90))
        );
        assert_eq!(
            next_of("https://forum.test/index.php?showtopic=123&st=90", IPB),
            None
        );
        // An http page keeps its http links.
        assert_eq!(
            next_of("http://forum.test/index.php?showtopic=123", IPB),
            Some(format!(
                "http://forum.test/index.php?{s}showtopic=123&st=30"
            ))
        );
        // An empty `s=` (IPB's logged-out form) on the current page.
        assert_eq!(
            next_of("https://forum.test/index.php?s=&showtopic=123&st=30", IPB),
            Some(want(60))
        );
    }

    #[test]
    fn ipb_act_st_form_and_other_step_sizes() {
        let html = r#"<a href="index.php?act=ST&amp;f=1&amp;t=55&amp;st=20">2</a>
            <a href="index.php?act=ST&amp;f=1&amp;t=55&amp;st=40">3</a>
            <a href="index.php?act=ST&amp;f=1&amp;t=56&amp;st=20">x</a>"#;
        assert_eq!(
            next_of("https://f.test/index.php?act=ST&f=1&t=55", html).as_deref(),
            Some("https://f.test/index.php?act=ST&f=1&t=55&st=20")
        );
        assert_eq!(
            next_of("https://f.test/index.php?act=ST&f=1&t=55&st=20", html).as_deref(),
            Some("https://f.test/index.php?act=ST&f=1&t=55&st=40")
        );
        assert_eq!(
            next_of("https://f.test/index.php?act=ST&f=1&t=55&st=40", html),
            None
        );
        // ?st= as the only query parameter, 25 per page.
        let html = r#"<a href="/topic.php?st=25">2</a><a href="/topic.php?st=50">3</a>"#;
        assert_eq!(
            next_of("https://f.test/topic.php", html).as_deref(),
            Some("https://f.test/topic.php?st=25")
        );
    }

    #[test]
    fn http_next_links_on_https_pages() {
        // rel=next, Link header and "Next" text all upgrade to https.
        let html = r#"<link rel="next" href="http://www.site.test/t/9?page=2">"#;
        assert_eq!(
            next_of("https://site.test/t/9", html).as_deref(),
            Some("https://www.site.test/t/9?page=2")
        );
        let html = r#"<a href="http://site.test/t/9?page=2">Next ›</a>"#;
        assert_eq!(
            next_of("https://site.test/t/9", html).as_deref(),
            Some("https://site.test/t/9?page=2")
        );
        let mut f = fetched("https://site.test/h", "");
        f.link_headers = vec![r#"<http://site.test/h?page=2>; rel="next""#.into()];
        assert_eq!(
            next_page(&f, &f.final_url)
                .map(|u| u.to_string())
                .as_deref(),
            Some("https://site.test/h?page=2")
        );
        // A rel=next to the http form of this very page is not a next page.
        let html = r#"<link rel="next" href="http://site.test/t/9#top">"#;
        assert_eq!(next_of("https://site.test/t/9", html), None);
        // An explicit other port is another site.
        let html = r#"<link rel="next" href="http://site.test:8080/t/9?page=2">"#;
        assert_eq!(next_of("https://site.test/t/9", html), None);
        // Same explicit port, other scheme: same site, kept on https.
        let html = r#"<a href="http://site.test:8443/t/9?page=2">2</a>"#;
        assert_eq!(
            next_of("https://site.test:8443/t/9", html).as_deref(),
            Some("https://site.test:8443/t/9?page=2")
        );
    }

    #[test]
    fn same_host_ignores_scheme_but_not_ports() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(same_host(
            &u("https://forum.test/"),
            &u("http://forum.test/x")
        ));
        assert!(same_host(
            &u("http://www.forum.test/"),
            &u("https://forum.test:443/")
        ));
        assert!(!same_host(
            &u("https://forum.test/"),
            &u("http://forum.test:8080/")
        ));
        assert!(!same_host(
            &u("https://forum.test/"),
            &u("ftp://forum.test/")
        ));
    }

    #[test]
    fn page_identity_ignores_scheme_and_session_ids_only() {
        let id = |s: &str| page_identity(&Url::parse(s).unwrap());
        let a = id("https://forum.test/index.php?showtopic=123&st=30");
        for same in [
            "http://forum.test/index.php?showtopic=123&st=30",
            "http://www.forum.test/index.php?s=0123456789abcdef0123456789abcdef&showtopic=123&st=30#entry9",
            "https://forum.test/index.php?s=&showtopic=123&st=30",
            "https://forum.test/index.php?showtopic=123&sid=ABCDEF0123456789ABCDEF0123456789&st=30",
            "https://forum.test/index.php?PHPSESSID=q7v0abc&showtopic=123&st=30",
        ] {
            assert_eq!(id(same), a, "{same}");
        }
        for other in [
            "https://forum.test/index.php?showtopic=123&st=60",
            "https://forum.test/index.php?showtopic=124&st=30",
            "https://forum.test:8443/index.php?showtopic=123&st=30",
        ] {
            assert_ne!(id(other), a, "{other}");
        }
        // Content that only looks like a session parameter is kept.
        assert_ne!(
            id("https://blog.test/?s=radios"),
            id("https://blog.test/?s=valves")
        );
        assert_ne!(
            id("https://nuke.test/modules.php?name=News&sid=12"),
            id("https://nuke.test/modules.php?name=News&sid=13")
        );
    }

    #[test]
    fn page_key_ignores_session_ids_and_scheme() {
        let k = |s: &str| page_key(&Url::parse(s).unwrap());
        let a = k("https://forum.test/index.php?showtopic=123");
        let b = k(
            "http://forum.test/index.php?s=0123456789abcdef0123456789abcdef&showtopic=123&st=30",
        );
        assert_eq!(a.base, b.base);
        assert_eq!((b.number, b.kind), (Some(30), PageKind::Offset));
        assert_ne!(
            a.base,
            k("https://forum.test/index.php?showtopic=124&st=30").base
        );
    }
}
