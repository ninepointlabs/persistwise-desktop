//! Pulls the title and readable text out of an HTML page, for the search
//! index. Scripts, styles and templates are dropped first.

use lol_html::html_content::{ContentType, Element, TextChunk};
use lol_html::{doc_text, element, text, RewriteStrSettings};
use std::cell::RefCell;

/// Most text indexed per page. Long pages are cut, not skipped.
pub const MAX_TEXT_BYTES: usize = 1_000_000;

pub struct Extracted {
    pub title: Option<String>,
    pub text: String,
}

pub fn extract(html: &str) -> Extracted {
    let title = RefCell::new(String::new());
    // Pass 1: drop non-content elements and capture the <title>.
    let stripped = lol_html::rewrite_str(
        html,
        lenient()
            .append_element_content_handler(
                element!("script, style, template, noscript, svg, head > *:not(title)", |el: &mut Element| {
                    el.remove();
                    Ok(())
                }))
            .append_element_content_handler(
                text!("title", |t: &mut TextChunk| {
                    title.borrow_mut().push_str(t.as_str());
                    Ok(())
                }))
            .append_element_content_handler(
                element!("title", |el: &mut Element| {
                    el.remove();
                    Ok(())
                }))
            // Block-level elements get a space around them so words from
            // neighbouring paragraphs do not run together.
            .append_element_content_handler(
                element!("p, div, br, li, h1, h2, h3, h4, h5, h6, td, th, tr, section, article, header, footer, blockquote, pre", |el: &mut Element| {
                    el.before(" ", ContentType::Text);
                    el.after(" ", ContentType::Text);
                    Ok(())
                })),
    )
    .unwrap_or_else(|_| html.to_string());

    let text = RefCell::new(String::new());
    let _ = lol_html::rewrite_str(
        &stripped,
        lenient().append_document_content_handler(doc_text!(|t| {
                let mut buf = text.borrow_mut();
                if buf.len() < MAX_TEXT_BYTES * 2 {
                    buf.push_str(t.as_str());
                }
                Ok(())
            })),
    );

    let text = collapse(&decode_entities(&text.into_inner()), MAX_TEXT_BYTES);
    let title = collapse(&decode_entities(&title.into_inner()), 500);
    Extracted {
        title: if title.is_empty() { None } else { Some(title) },
        text,
    }
}

pub(crate) fn lenient<'h, 's>() -> RewriteStrSettings<'h, 's> {
    RewriteStrSettings::new().with_strict(false).with_enable_esi_tags(false)
}

fn collapse(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max));
    for word in s.split_whitespace() {
        if out.len() + word.len() + 1 > max {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Decodes the handful of entities that matter for search. Text chunks from
/// the rewriter are raw source text, so `&amp;` arrives as written.
fn decode_entities(s: &str) -> String {
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
            _ if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32).unwrap_or(' '),
            _ => e[1..].parse::<u32>().ok().and_then(char::from_u32).unwrap_or(' '),
        };
        ch.to_string()
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_and_body_without_scripts() {
        let html = r#"<!doctype html><html><head><title>Hello &amp; welcome</title>
            <style>.x{color:red}</style><script>var secret = 1;</script></head>
            <body><h1>Heading</h1><p>First para.</p><p>Second&nbsp;para.</p>
            <script>alert('no')</script><noscript>nojs</noscript></body></html>"#;
        let e = extract(html);
        assert_eq!(e.title.as_deref(), Some("Hello & welcome"));
        assert!(e.text.contains("Heading"));
        assert!(e.text.contains("First para. Second para."));
        assert!(!e.text.contains("secret"));
        assert!(!e.text.contains("color:red"));
        assert!(!e.text.contains("alert"));
        assert!(!e.text.contains("Hello & welcome"), "title is stored separately");
    }
}
