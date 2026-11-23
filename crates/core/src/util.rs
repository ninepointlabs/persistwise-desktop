//! Small helpers shared by the other modules.

use sha2::{Digest, Sha256};
use url::Url;

/// Current time as an RFC 3339 UTC string, the format every timestamp in
/// the database uses.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// A random 128-bit id as 32 lowercase hex characters. Used for page ids
/// that stay stable across devices and imports.
pub fn new_uid() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Resolves `value` against `base`, leaving alone things that should not be
/// touched: empty values, same-page fragments, and anything that already has
/// a scheme (https:, data:, mailto:, javascript:).
pub fn absolutize(value: &str, base: &Url) -> Option<String> {
    let v = value.trim();
    if v.is_empty() || v.starts_with('#') {
        return None;
    }
    if has_scheme(v) {
        return None;
    }
    base.join(v).ok().map(|u| u.to_string())
}

pub fn has_scheme(v: &str) -> bool {
    let mut chars = v.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    for c in chars {
        if c == ':' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
            return false;
        }
    }
    false
}

/// Decodes HTML bytes to a String. Honours, in order: a BOM, the charset in
/// the Content-Type header, a `<meta charset>` in the first 2 KB, then
/// UTF-8 (lossy).
pub fn decode_html(bytes: &[u8], content_type: Option<&str>) -> String {
    if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        let (s, _) = enc.decode_without_bom_handling(&bytes[bom_len..]);
        return s.into_owned();
    }
    let label = content_type
        .and_then(charset_from_content_type)
        .or_else(|| meta_charset(bytes));
    if let Some(enc) = label.and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes())) {
        let (s, _) = enc.decode_without_bom_handling(bytes);
        return s.into_owned();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn charset_from_content_type(ct: &str) -> Option<String> {
    ct.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        if k.trim().eq_ignore_ascii_case("charset") {
            Some(v.trim().trim_matches('"').to_string())
        } else {
            None
        }
    })
}

fn meta_charset(bytes: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_ascii_lowercase();
    let re = regex::Regex::new(r#"<meta[^>]+charset\s*=\s*["']?([a-z0-9_\-:.]+)"#).ok()?;
    re.captures(&head).map(|c| c[1].to_string())
}

/// True when the bytes look like an HTML document.
pub fn looks_like_html(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    let head = head.trim_start();
    head.starts_with("<!doctype html")
        || head.contains("<html")
        || head.contains("<head")
        || head.contains("<body")
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Shows only the last four characters of a secret.
pub fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 4 {
        return "••••".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••{tail}")
}

/// Decodes the handful of entities that matter for search. Text chunks from
/// the rewriter (and attribute values) are raw source text, so `&amp;` arrives as written.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let re = regex::Regex::new(r"&(#x[0-9a-fA-F]+|#[0-9]+|amp|lt|gt|quot|apos|nbsp|mdash|ndash|hellip|rsquo|lsquo|rdquo|ldquo);").unwrap();
    re.replace_all(s, |c: &regex::Captures| {
        let e = &c[1];
        let ch = match e {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" | "rsquo" | "lsquo" => '\'',
            "rdquo" | "ldquo" => '"',
            "nbsp" => ' ',
            "mdash" => '—',
            "ndash" => '–',
            "hellip" => '…',
            _ if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32)
                .unwrap_or(' '),
            _ => e[1..]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .unwrap_or(' '),
        };
        ch.to_string()
    })
    .into_owned()
}

/// Makes a value safe to hand to lol_html's `set_attribute`, which only
/// escapes quotes.
pub fn attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolutize_leaves_schemes_and_fragments() {
        let base = Url::parse("https://example.com/a/b.html").unwrap();
        assert_eq!(
            absolutize("img.png", &base).unwrap(),
            "https://example.com/a/img.png"
        );
        assert_eq!(
            absolutize("/x.css", &base).unwrap(),
            "https://example.com/x.css"
        );
        assert_eq!(
            absolutize("//cdn.test/y.js", &base).unwrap(),
            "https://cdn.test/y.js"
        );
        assert!(absolutize("data:image/png;base64,AAA", &base).is_none());
        assert!(absolutize("mailto:a@b.c", &base).is_none());
        assert!(absolutize("#top", &base).is_none());
        assert!(absolutize("  ", &base).is_none());
    }

    #[test]
    fn decode_honours_meta_charset() {
        let bytes = b"<html><head><meta charset=\"iso-8859-1\"></head><body>caf\xe9</body></html>";
        assert!(decode_html(bytes, None).contains("café"));
        assert!(
            decode_html("<p>café</p>".as_bytes(), Some("text/html; charset=utf-8"))
                .contains("café")
        );
    }

    #[test]
    fn entities_round_trip_for_attributes() {
        assert_eq!(
            decode_entities("/w/load.php?lang=en&amp;modules=a%7Cb&#38;x=1"),
            "/w/load.php?lang=en&modules=a%7Cb&x=1"
        );
        assert_eq!(attr_escape("a?b=1&c=2"), "a?b=1&amp;c=2");
    }

    #[test]
    fn mask_shows_last_four_only() {
        assert_eq!(mask_secret("abcdefgh1234"), "••••1234");
        assert_eq!(mask_secret("abc"), "••••");
    }
}
