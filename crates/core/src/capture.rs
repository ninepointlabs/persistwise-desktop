//! Saves a full, self-contained copy of a web page.
//!
//! The page is fetched once, then its stylesheets, images, icons and fonts
//! are fetched and inlined as `data:` URIs, so the saved file opens offline
//! with no network requests. Scripts are removed (a saved copy is a static
//! snapshot, and scripts are what usually break or phone home). Links that
//! cannot be inlined are made absolute. Non-HTML responses (PDFs, images,
//! plain text) are kept byte for byte.

use anyhow::{bail, Context, Result};
use base64::Engine;
use lol_html::html_content::{ContentType, Element, TextChunk};
use lol_html::{element, text};
use regex::Regex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use url::Url;

use crate::{http, text as textx, util};

#[derive(Debug, Clone)]
pub struct CaptureOptions {
    pub max_page_bytes: usize,
    pub max_asset_bytes: usize,
    pub max_total_bytes: usize,
    pub max_assets: usize,
    pub workers: usize,
    pub timeout: Duration,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            max_page_bytes: 25 * 1024 * 1024,
            max_asset_bytes: 8 * 1024 * 1024,
            max_total_bytes: 60 * 1024 * 1024,
            max_assets: 400,
            workers: 6,
            timeout: Duration::from_secs(45),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Captured {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub ext: &'static str,
    pub final_url: String,
    pub title: Option<String>,
    pub text: String,
    pub assets_inlined: usize,
    pub assets_failed: usize,
}

#[derive(Clone)]
struct Asset {
    mime: String,
    bytes: Arc<Vec<u8>>,
}

/// Fetches and caches page resources within the size budget.
struct Fetcher {
    client: reqwest::blocking::Client,
    opts: CaptureOptions,
    cache: Mutex<HashMap<String, Option<Asset>>>,
    used_bytes: Mutex<usize>,
}

impl Fetcher {
    fn get(&self, url: &str) -> Option<Asset> {
        self.cache.lock().unwrap().get(url).cloned().flatten()
    }

    fn fetch_one(&self, url: &str) -> Option<Asset> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return None;
        }
        {
            let used = *self.used_bytes.lock().unwrap();
            if used >= self.opts.max_total_bytes {
                return None;
            }
        }
        let resp = self.client.get(url).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| guess_mime(url).to_string());
        let bytes = http::read_limited(resp, self.opts.max_asset_bytes).ok()?;
        let mut used = self.used_bytes.lock().unwrap();
        if *used + bytes.len() > self.opts.max_total_bytes {
            return None;
        }
        *used += bytes.len();
        Some(Asset {
            mime,
            bytes: Arc::new(bytes),
        })
    }

    /// Fetches every URL not already cached, a few at a time.
    fn fetch_all(&self, urls: Vec<String>) {
        let mut todo: Vec<String> = {
            let cache = self.cache.lock().unwrap();
            let mut v: Vec<String> = urls
                .into_iter()
                .filter(|u| !cache.contains_key(u))
                .collect();
            v.sort();
            v.dedup();
            v
        };
        let room = self
            .opts
            .max_assets
            .saturating_sub(self.cache.lock().unwrap().len());
        if todo.len() > room {
            for u in todo.split_off(room) {
                self.cache.lock().unwrap().insert(u, None);
            }
        }
        if todo.is_empty() {
            return;
        }
        let queue = Mutex::new(todo);
        std::thread::scope(|s| {
            for _ in 0..self.opts.workers.max(1) {
                s.spawn(|| loop {
                    let next = queue.lock().unwrap().pop();
                    let Some(url) = next else { break };
                    let asset = self.fetch_one(&url);
                    self.cache.lock().unwrap().insert(url, asset);
                });
            }
        });
    }

    fn data_uri(&self, url: &str) -> Option<String> {
        let a = self.get(url)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(a.bytes.as_slice());
        Some(format!("data:{};base64,{}", a.mime, b64))
    }

    fn css_text(&self, url: &str) -> Option<String> {
        let a = self.get(url)?;
        Some(util::decode_html(&a.bytes, Some("text/css")))
    }

    // ---- CSS ----

    /// Fetches everything a stylesheet refers to (recursively for @import).
    fn prefetch_css(&self, css: &str, base: &Url, depth: usize) {
        let (imports, urls) = css_refs(css, base);
        if depth < 3 && !imports.is_empty() {
            self.fetch_all(imports.clone());
            for imp in &imports {
                if let (Some(text), Ok(u)) = (self.css_text(imp), Url::parse(imp)) {
                    self.prefetch_css(&text, &u, depth + 1);
                }
            }
        }
        self.fetch_all(urls);
    }

    /// Rewrites a stylesheet from the cache: url()s become data: URIs (or
    /// absolute URLs when they could not be fetched), @imports become
    /// inlined data: stylesheets.
    fn rewrite_css(&self, css: &str, base: &Url, depth: usize) -> String {
        let css = import_re().replace_all(css, |c: &regex::Captures| {
            let raw = (1..=5)
                .find_map(|i| c.get(i))
                .map(|m| m.as_str())
                .unwrap_or("");
            let rest = c.get(6).map(|m| m.as_str()).unwrap_or("");
            let Some(abs) = resolve(raw, base) else {
                return c[0].to_string();
            };
            if depth < 3 {
                if let (Some(text), Ok(u)) = (self.css_text(&abs), Url::parse(&abs)) {
                    let inner = self.rewrite_css(&text, &u, depth + 1);
                    let b64 = base64::engine::general_purpose::STANDARD.encode(inner.as_bytes());
                    return format!("@import url(\"data:text/css;base64,{b64}\"){rest}");
                }
            }
            format!("@import url(\"{}\"){rest}", css_escape(&abs))
        });
        url_re()
            .replace_all(&css, |c: &regex::Captures| {
                let raw = c
                    .get(1)
                    .or_else(|| c.get(2))
                    .or_else(|| c.get(3))
                    .map(|m| m.as_str())
                    .unwrap_or("");
                match resolve(raw, base) {
                    Some(abs) => match self.data_uri(&abs) {
                        Some(d) => format!("url(\"{d}\")"),
                        None => format!("url(\"{}\")", css_escape(&abs)),
                    },
                    None => c[0].to_string(),
                }
            })
            .into_owned()
    }
}

fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)url\(\s*(?:"([^"]*)"|'([^']*)'|([^)'"\s]*))\s*\)"#).unwrap()
    })
}

fn import_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)@import\s+(?:url\(\s*(?:"([^"]*)"|'([^']*)'|([^)'"\s]*))\s*\)|"([^"]*)"|'([^']*)')([^;]*);"#)
            .unwrap()
    })
}

/// Returns (imported stylesheet URLs, other url() references).
fn css_refs(css: &str, base: &Url) -> (Vec<String>, Vec<String>) {
    let mut imports = Vec::new();
    for c in import_re().captures_iter(css) {
        let raw = (1..=5)
            .find_map(|i| c.get(i))
            .map(|m| m.as_str())
            .unwrap_or("");
        if let Some(a) = resolve(raw, base) {
            imports.push(a);
        }
    }
    let without_imports = import_re().replace_all(css, "");
    let urls = url_re()
        .captures_iter(&without_imports)
        .filter_map(|c| {
            let raw = c.get(1).or_else(|| c.get(2)).or_else(|| c.get(3))?.as_str();
            resolve(raw, base)
        })
        .collect();
    (imports, urls)
}

fn resolve(raw: &str, base: &Url) -> Option<String> {
    let v = raw.trim();
    if v.is_empty() || v.starts_with('#') {
        return None;
    }
    if util::has_scheme(v) {
        return (v.starts_with("http://") || v.starts_with("https://")).then(|| v.to_string());
    }
    base.join(v).ok().map(|u| u.to_string())
}

fn css_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "")
}

fn guess_mime(url: &str) -> &'static str {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "css" => "text/css",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        _ => "application/octet-stream",
    }
}

/// File extension for a saved non-HTML response.
pub fn ext_for(content_type: &str) -> &'static str {
    match content_type {
        "text/html" | "application/xhtml+xml" => "html",
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "text/plain" => "txt",
        "application/json" => "json",
        "application/xml" | "text/xml" | "application/rss+xml" | "application/atom+xml" => "xml",
        _ => "bin",
    }
}

/// Picks the largest candidate from a srcset.
fn best_srcset(srcset: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(\S+?)(?:\s+([\d.]+)([wx]))?\s*(?:,\s*|$)").unwrap());
    re.captures_iter(srcset.trim())
        .filter(|c| !c[1].starts_with("data:"))
        .map(|c| {
            let n: f64 = c
                .get(2)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(1.0);
            (n, c[1].trim_end_matches(',').to_string())
        })
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, u)| u)
}

/// The image URL an <img> really shows, looking past lazy-loading
/// placeholders (`data-src` and friends), which never load without scripts.
fn img_source(el: &Element) -> Option<String> {
    let src = attr(el, "src").filter(|s| !s.trim().is_empty());
    let lazy = ["data-src", "data-lazy-src", "data-original", "data-url"]
        .iter()
        .find_map(|a| attr(el, a))
        .filter(|s| !s.trim().is_empty());
    match (src, lazy) {
        (Some(s), Some(l)) if s.starts_with("data:") => Some(l),
        (Some(s), _) => Some(s),
        (None, Some(l)) => Some(l),
        (None, None) => el
            .get_attribute("srcset")
            .or_else(|| attr(el, "data-srcset"))
            .and_then(|s| best_srcset(&s)),
    }
}

/// An attribute value with entities decoded (lol_html returns raw source).
fn attr(el: &Element, name: &str) -> Option<String> {
    el.get_attribute(name).map(|v| util::decode_entities(&v))
}

/// Sets an attribute, escaping `&` (lol_html only escapes quotes).
fn set(el: &mut Element, name: &str, value: &str) {
    let _ = el.set_attribute(name, &util::attr_escape(value));
}

fn is_rel(el: &Element, want: &[&str]) -> bool {
    attr(el, "rel")
        .map(|r| {
            r.split_ascii_whitespace()
                .any(|t| want.iter().any(|w| t.eq_ignore_ascii_case(w)))
        })
        .unwrap_or(false)
}

/// Saves `url`. Fails only if the page itself cannot be fetched.
pub fn capture(url: &str, opts: &CaptureOptions) -> Result<Captured> {
    let parsed = Url::parse(url.trim()).context("that does not look like a web address")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("only http and https pages can be saved");
    }
    let client = http::client(opts.timeout)?;
    let resp = client.get(parsed.as_str()).send().with_context(|| {
        format!(
            "could not reach {}",
            parsed.host_str().unwrap_or("the site")
        )
    })?;
    let status = resp.status();
    if !status.is_success() {
        bail!("the site answered HTTP {}", status.as_u16());
    }
    let final_url = resp.url().clone();
    let header_ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mime = header_ct
        .as_deref()
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    let bytes = http::read_limited(resp, opts.max_page_bytes)?;

    let is_html = mime.contains("html") || (mime.is_empty() && util::looks_like_html(&bytes));
    if !is_html {
        let ct = if mime.is_empty() {
            "application/octet-stream".to_string()
        } else {
            mime
        };
        let name = final_url
            .path_segments()
            .and_then(|mut s| s.next_back().map(str::to_string))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| final_url.host_str().unwrap_or("download").to_string());
        let text = if ct.starts_with("text/") {
            String::from_utf8_lossy(&bytes)
                .chars()
                .take(textx::MAX_TEXT_BYTES)
                .collect()
        } else {
            String::new()
        };
        return Ok(Captured {
            ext: ext_for(&ct),
            content_type: ct,
            final_url: final_url.to_string(),
            title: Some(name),
            text,
            bytes,
            assets_inlined: 0,
            assets_failed: 0,
        });
    }

    let html = util::decode_html(&bytes, header_ct.as_deref());
    let extracted = textx::extract(&html);
    let out = inline_page(&html, &final_url, client, opts)?;
    Ok(Captured {
        bytes: out.0.into_bytes(),
        content_type: "text/html".into(),
        ext: "html",
        final_url: final_url.to_string(),
        title: extracted.title,
        text: extracted.text,
        assets_inlined: out.1,
        assets_failed: out.2,
    })
}

/// Rewrites an already-fetched HTML document into a self-contained one.
/// Returns (html, assets inlined, assets that could not be inlined).
pub fn inline_page(
    html: &str,
    page_url: &Url,
    client: reqwest::blocking::Client,
    opts: &CaptureOptions,
) -> Result<(String, usize, usize)> {
    // Pass 1: collect what the page refers to.
    let base_href: RefCell<Option<String>> = RefCell::new(None);
    let stylesheets: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let images: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let inline_css: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let style_buf = RefCell::new(String::new());

    lol_html::rewrite_str(
        html,
        textx::lenient()
            .append_element_content_handler(element!("base[href]", |el: &mut Element| {
                let mut b = base_href.borrow_mut();
                if b.is_none() {
                    *b = attr(el, "href");
                }
                Ok(())
            }))
            .append_element_content_handler(element!("link[href]", |el: &mut Element| {
                let href = attr(el, "href").unwrap_or_default();
                if is_rel(el, &["stylesheet"]) {
                    stylesheets.borrow_mut().push(href);
                } else if is_rel(el, &["icon", "apple-touch-icon"]) {
                    images.borrow_mut().push(href);
                }
                Ok(())
            }))
            .append_element_content_handler(element!(
                "img, input[type=image]",
                |el: &mut Element| {
                    if let Some(s) = img_source(el) {
                        images.borrow_mut().push(s);
                    }
                    Ok(())
                }
            ))
            .append_element_content_handler(element!("video[poster]", |el: &mut Element| {
                images
                    .borrow_mut()
                    .push(attr(el, "poster").unwrap_or_default());
                Ok(())
            }))
            .append_element_content_handler(element!("[background]", |el: &mut Element| {
                images
                    .borrow_mut()
                    .push(attr(el, "background").unwrap_or_default());
                Ok(())
            }))
            .append_element_content_handler(element!("[style]", |el: &mut Element| {
                inline_css
                    .borrow_mut()
                    .push(attr(el, "style").unwrap_or_default());
                Ok(())
            }))
            .append_element_content_handler(text!("style", |t: &mut TextChunk| {
                style_buf.borrow_mut().push_str(t.as_str());
                if t.last_in_text_node() {
                    let css = std::mem::take(&mut *style_buf.borrow_mut());
                    inline_css.borrow_mut().push(css);
                }
                Ok(())
            })),
    )
    .map_err(|e| anyhow::anyhow!("could not read the page's HTML: {e}"))?;

    let base = base_href
        .into_inner()
        .and_then(|b| page_url.join(b.trim()).ok())
        .unwrap_or_else(|| page_url.clone());

    let fetcher = Fetcher {
        client,
        opts: opts.clone(),
        cache: Mutex::new(HashMap::new()),
        used_bytes: Mutex::new(0),
    };

    // Stylesheets first, then everything they and the page refer to.
    let sheet_urls: Vec<String> = stylesheets
        .borrow()
        .iter()
        .filter_map(|s| resolve(s, &base))
        .collect();
    fetcher.fetch_all(sheet_urls.clone());
    for s in &sheet_urls {
        if let (Some(text), Ok(u)) = (fetcher.css_text(s), Url::parse(s)) {
            fetcher.prefetch_css(&text, &u, 0);
        }
    }
    for css in inline_css.borrow().iter() {
        fetcher.prefetch_css(css, &base, 0);
    }
    fetcher.fetch_all(
        images
            .borrow()
            .iter()
            .filter_map(|s| resolve(s, &base))
            .collect(),
    );

    let processed_sheets: HashMap<String, String> = sheet_urls
        .iter()
        .filter_map(|s| {
            let text = fetcher.css_text(s)?;
            let u = Url::parse(s).ok()?;
            Some((s.clone(), fetcher.rewrite_css(&text, &u, 0)))
        })
        .collect();

    // Pass 2: rewrite.
    let style_buf = RefCell::new(String::new());
    let absolutize_attr = |el: &mut Element, name: &str| {
        if let Some(v) = attr(el, name) {
            if let Some(abs) = util::absolutize(&v, &base) {
                set(el, name, &abs);
            }
        }
    };
    let saved_meta = format!(
        "<meta charset=\"utf-8\"><meta name=\"persistwise:saved-from\" content=\"{}\"><meta name=\"persistwise:saved-at\" content=\"{}\">",
        util::html_escape(page_url.as_str()),
        util::now()
    );

    let out = lol_html::rewrite_str(
        html,
        textx::lenient()
            .append_element_content_handler(element!("script, base, link[rel~=preload i], link[rel~=prefetch i], link[rel~=modulepreload i], link[rel~=preconnect i], link[rel~=dns-prefetch i], link[rel~=manifest i], meta[charset]", |el: &mut Element| {
                el.remove();
                Ok(())
            }))
            .append_element_content_handler(element!("meta[http-equiv]", |el: &mut Element| {
                let he = attr(el, "http-equiv").unwrap_or_default().to_ascii_lowercase();
                if matches!(he.as_str(), "refresh" | "content-security-policy" | "content-type" | "set-cookie") {
                    el.remove();
                }
                Ok(())
            }))
            .append_element_content_handler(element!("head", |el: &mut Element| {
                el.prepend(&saved_meta, ContentType::Html);
                Ok(())
            }))
            .append_element_content_handler(element!("*", |el: &mut Element| {
                let handlers: Vec<String> = el
                    .attributes()
                    .iter()
                    .map(|a| a.name())
                    .filter(|n| n.starts_with("on"))
                    .collect();
                for n in handlers {
                    el.remove_attribute(&n);
                }
                for name in ["href", "src", "action", "cite", "data", "formaction"] {
                    if let Some(v) = attr(el, name) {
                        if v.trim_start().to_ascii_lowercase().starts_with("javascript:") {
                            el.remove_attribute(name);
                        }
                    }
                }
                Ok(())
            }))
            .append_element_content_handler(element!("link[href]", |el: &mut Element| {
                let href = attr(el, "href").unwrap_or_default();
                let abs = resolve(&href, &base);
                if is_rel(el, &["stylesheet"]) {
                    if let Some(css) = abs.as_ref().and_then(|a| processed_sheets.get(a)) {
                        let media = el
                            .get_attribute("media")
                            .map(|m| format!(" media=\"{}\"", util::html_escape(&m)))
                            .unwrap_or_default();
                        let css = css.replace("</", "<\\/");
                        el.replace(&format!("<style{media}>{css}</style>"), ContentType::Html);
                        return Ok(());
                    }
                } else if is_rel(el, &["icon", "apple-touch-icon"]) {
                    if let Some(d) = abs.as_ref().and_then(|a| fetcher.data_uri(a)) {
                        set(el, "href", &d);
                        return Ok(());
                    }
                }
                if let Some(a) = abs {
                    set(el, "href", &a);
                }
                Ok(())
            }))
            .append_element_content_handler(element!("img, input[type=image]", |el: &mut Element| {
                let chosen = img_source(el);
                for a in ["data-src", "data-lazy-src", "data-original", "data-url", "data-srcset", "loading"] {
                    el.remove_attribute(a);
                }
                if let Some(src) = chosen.as_deref().and_then(|s| resolve(s, &base)) {
                    match fetcher.data_uri(&src) {
                        Some(d) => {
                            set(el, "src", &d);
                            el.remove_attribute("srcset");
                            el.remove_attribute("sizes");
                        }
                        None => {
                            set(el, "src", &src);
                        }
                    }
                }
                if let Some(ss) = attr(el, "srcset") {
                    let fixed: Vec<String> = ss
                        .split(',')
                        .map(|c| {
                            let c = c.trim();
                            let (u, d) = c.split_once(char::is_whitespace).unwrap_or((c, ""));
                            let u = util::absolutize(u, &base).unwrap_or_else(|| u.to_string());
                            if d.is_empty() { u } else { format!("{u} {}", d.trim()) }
                        })
                        .collect();
                    set(el, "srcset", &fixed.join(", "));
                }
                Ok(())
            }))
            .append_element_content_handler(element!("picture > source[srcset]", |el: &mut Element| {
                // The <img> fallback inside <picture> carries the inlined image.
                el.remove();
                Ok(())
            }))
            .append_element_content_handler(element!("video[poster]", |el: &mut Element| {
                if let Some(src) = attr(el, "poster").and_then(|p| resolve(&p, &base)) {
                    let v = fetcher.data_uri(&src).unwrap_or(src);
                    set(el, "poster", &v);
                }
                Ok(())
            }))
            .append_element_content_handler(element!("[background]", |el: &mut Element| {
                if let Some(src) = attr(el, "background").and_then(|p| resolve(&p, &base)) {
                    let v = fetcher.data_uri(&src).unwrap_or(src);
                    set(el, "background", &v);
                }
                Ok(())
            }))
            .append_element_content_handler(element!("a[href], area[href]", |el: &mut Element| {
                absolutize_attr(el, "href");
                set(el, "target", "_blank");
                set(el, "rel", "noopener noreferrer");
                Ok(())
            }))
            .append_element_content_handler(element!("iframe[src], frame[src], embed[src], audio[src], video[src], source[src], track[src], object[data], form[action], blockquote[cite], q[cite]", |el: &mut Element| {
                for name in ["src", "data", "action", "cite"] {
                    absolutize_attr(el, name);
                }
                Ok(())
            }))
            .append_element_content_handler(element!("[style]", |el: &mut Element| {
                if let Some(s) = attr(el, "style") {
                    set(el, "style", &fetcher.rewrite_css(&s, &base, 0));
                }
                Ok(())
            }))
            .append_element_content_handler(text!("style", |t: &mut TextChunk| {
                style_buf.borrow_mut().push_str(t.as_str());
                if t.last_in_text_node() {
                    let css = std::mem::take(&mut *style_buf.borrow_mut());
                    let rewritten = fetcher.rewrite_css(&css, &base, 0).replace("</", "<\\/");
                    t.replace(&rewritten, ContentType::Html);
                } else {
                    t.remove();
                }
                Ok(())
            })),
    )
    .map_err(|e| anyhow::anyhow!("could not rewrite the page: {e}"))?;

    let cache = fetcher.cache.lock().unwrap();
    let inlined = cache.values().filter(|v| v.is_some()).count();
    let failed = cache.values().filter(|v| v.is_none()).count();
    Ok((out, inlined, failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srcset_picks_largest() {
        assert_eq!(
            best_srcset("a.jpg 320w, b.jpg 1024w, c.jpg 640w").as_deref(),
            Some("b.jpg")
        );
        assert_eq!(best_srcset("a.jpg 1x, b.jpg 2x").as_deref(), Some("b.jpg"));
        assert_eq!(best_srcset("only.jpg").as_deref(), Some("only.jpg"));
    }

    #[test]
    fn css_refs_finds_imports_and_urls() {
        let base = Url::parse("https://x.test/css/site.css").unwrap();
        let (imps, urls) = css_refs(
            r#"@import "reset.css"; @import url('/theme.css') screen; body{background:url(bg.png)} .a{background:url("data:image/png;base64,AA")}"#,
            &base,
        );
        assert_eq!(
            imps,
            vec!["https://x.test/css/reset.css", "https://x.test/theme.css"]
        );
        assert_eq!(urls, vec!["https://x.test/css/bg.png"]);
    }
}
