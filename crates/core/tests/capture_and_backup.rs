mod common;

use common::{Reply, Server, PNG};
use persistwise_core::capture::{capture, CaptureOptions};
use persistwise_core::settings::{self, IpfsMode, Secrets};
use persistwise_core::store::Store;
use persistwise_core::wayback::Wayback;
use persistwise_core::{backup_ipfs, backup_wayback, ipfs, save_page};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const PAGE: &str = r#"<!doctype html>
<html><head>
<meta charset="windows-1252">
<meta http-equiv="refresh" content="0; url=/elsewhere">
<title>Field notes</title>
<base href="/blog/">
<link rel="stylesheet" href="../css/site.css?v=1&amp;theme=dark" media="screen">
<link rel="preload" href="/big.js" as="script">
<link rel="icon" href="/favicon.png">
<script src="/tracker.js"></script>
<script>document.write('tracked')</script>
<style>.hero { background-image: url('hero.png'); }</style>
</head>
<body onload="evil()">
<h1 style="background: url(/inline.png)">Quiet rivers</h1>
<p>Herons wait in the reeds.</p>
<img src="photo.png" srcset="photo.png 1x, photo@2x.png 2x" alt="a heron">
<img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" data-src="lazy.png" alt="lazy">
<picture><source srcset="/wide.png" media="(min-width: 800px)"><img src="/narrow.png"></picture>
<a href="next.html">Next</a> <a href="javascript:alert(1)">bad</a>
<img src="/missing.png">
</body></html>"#;

fn site() -> Server {
    Server::start(|_m, url| {
        match url {
        "/blog/post" => Reply::ok("text/html", PAGE),
        "/css/site.css?v=1&theme=dark" => Reply::ok("text/css", "@import 'reset.css';\n@font-face{font-family:F;src:url(font.woff2) format('woff2')}\nbody{background:url(/bg.png)}"),
        "/css/reset.css" => Reply::ok("text/css", "html{margin:0;background:url(dots.png)}"),
        "/css/font.woff2" => Reply::ok("font/woff2", b"wOF2fakefont".to_vec()),
        "/report.pdf" => Reply::ok("application/pdf", b"%PDF-1.4 fake".to_vec()),
        "/gone" => Reply::status(404),
        u if u.ends_with(".png") && u != "/missing.png" => Reply::ok("image/png", PNG.to_vec()),
        _ => Reply::status(404),
    }
    })
}

#[test]
fn capture_makes_a_self_contained_copy() {
    let s = site();
    let c = capture(&format!("{}/blog/post", s.base), &CaptureOptions::default()).unwrap();
    let html = String::from_utf8(c.bytes.clone()).unwrap();

    assert_eq!(c.content_type, "text/html");
    assert_eq!(c.title.as_deref(), Some("Field notes"));
    assert!(c.text.contains("Herons wait in the reeds."));

    // Scripts, handlers, refresh and preload hints are gone.
    assert!(!html.contains("<script"), "{html}");
    assert!(!html.contains("tracker.js"));
    assert!(!html.contains("onload"));
    assert!(!html.contains("http-equiv=\"refresh\""));
    assert!(!html.contains("big.js"));
    assert!(!html.contains("javascript:"));
    assert!(!html.contains("<base"));

    // Stylesheet inlined, with its @import, font and background inlined too.
    assert!(html.contains("<style media=\"screen\">"));
    assert!(html.contains("@import url(\"data:text/css;base64,"));
    assert!(html.contains("data:font/woff2;base64,"));
    // Images, the lazy-loaded one, icons and CSS backgrounds are inline.
    assert!(html.contains("data:image/png;base64,"));
    assert!(
        !html.contains("lazy.png"),
        "lazy image should be inlined, not linked; failures: {:#?}",
        c.failed_assets
    );
    assert!(
        !html.contains("hero.png") && !html.contains("inline.png") && !html.contains("favicon.png")
    );
    assert!(
        !html.contains("srcset=\"photo"),
        "srcset dropped once src is inlined"
    );
    assert!(
        !html.contains("wide.png"),
        "picture <source> dropped in favour of the inlined <img>"
    );

    // Links are absolute and inert; a missing image stays an absolute link.
    assert!(html.contains(&format!("href=\"{}/blog/next.html\"", s.base)));
    assert!(html.contains("target=\"_blank\""));
    assert!(html.contains(&format!("src=\"{}/missing.png\"", s.base)));
    assert!(html.contains("<meta charset=\"utf-8\">"));
    assert!(!html.contains("windows-1252"));

    assert!(c.assets_inlined >= 8, "inlined {}", c.assets_inlined);
    assert_eq!(c.assets_failed, 1);
    // The page plus each asset fetched once (no duplicate downloads).
    let photo_hits = s.requests_to("/blog/photo.png").len();
    assert_eq!(photo_hits, 1);
}

#[test]
fn save_page_stores_and_indexes_and_survives_without_network_backups() {
    let s = site();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let page = save_page(
        &store,
        &format!("{}/blog/post", s.base),
        &CaptureOptions::default(),
    )
    .unwrap();

    assert_eq!(page.wayback_status, "off");
    assert_eq!(page.ipfs_status, "off");
    let path = store.file_path(&page).unwrap();
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("Quiet rivers"));
    assert_eq!(page.sha256.as_deref().map(str::len), Some(64));

    let hits = store.list_pages(Some("heron"), None, 10, 0).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title.as_deref(), Some("Field notes"));

    // IPFS not configured: the IPFS step is a clean no-op.
    let after = backup_ipfs(&store, page.id).unwrap();
    assert_eq!(after.ipfs_status, "off");
    assert!(after.ipfs_error.is_none());
}

#[test]
fn non_html_is_kept_byte_for_byte() {
    let s = site();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let page = save_page(
        &store,
        &format!("{}/report.pdf", s.base),
        &CaptureOptions::default(),
    )
    .unwrap();
    assert_eq!(page.content_type, "application/pdf");
    assert!(page.local_path.as_deref().unwrap().ends_with(".pdf"));
    assert_eq!(
        std::fs::read(store.file_path(&page).unwrap()).unwrap(),
        b"%PDF-1.4 fake"
    );
}

#[test]
fn failed_fetch_saves_nothing() {
    let s = site();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let err = save_page(
        &store,
        &format!("{}/gone", s.base),
        &CaptureOptions::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("404"), "{err}");
    assert!(save_page(&store, "ftp://example.com/x", &CaptureOptions::default()).is_err());
    assert!(save_page(&store, "not a url", &CaptureOptions::default()).is_err());
    assert_eq!(store.stats().unwrap().pages, 0);
}

fn fast_wayback(base: &str) -> Wayback {
    Wayback {
        base: base.into(),
        availability_base: base.into(),
        poll_interval: Duration::from_millis(10),
        max_polls: 5,
        timeout: Duration::from_secs(5),
    }
}

#[test]
fn wayback_redirect_answer() {
    let spn = Server::start(|_m, url| {
        assert!(url.starts_with("/save/https://example.org/a"));
        Reply::status(302).header(
            "Location",
            "https://web.archive.org/web/20261123120000/https://example.org/a",
        )
    });
    let url = fast_wayback(&spn.base)
        .save("https://example.org/a")
        .unwrap();
    assert_eq!(
        url,
        "https://web.archive.org/web/20261123120000/https://example.org/a"
    );
}

#[test]
fn wayback_job_polling_answer() {
    let polls = Arc::new(AtomicUsize::new(0));
    let p = polls.clone();
    let spn = Server::start(move |_m, url| {
        if url.starts_with("/save/status/spn2-") {
            if p.fetch_add(1, Ordering::SeqCst) < 2 {
                Reply::ok("application/json", r#"{"status":"pending"}"#)
            } else {
                Reply::ok(
                    "application/json",
                    r#"{"status":"success","timestamp":"20261123121314","original_url":"https://example.org/b"}"#,
                )
            }
        } else {
            Reply::ok(
                "text/html",
                r#"<html><script>spn.watchJob("spn2-0123456789abcdef0123456789abcdef", "/_static/", 6000);</script></html>"#,
            )
        }
    });
    let url = fast_wayback(&spn.base)
        .save("https://example.org/b")
        .unwrap();
    assert_eq!(
        url,
        format!("{}/web/20261123121314/https://example.org/b", spn.base)
    );
    assert_eq!(polls.load(Ordering::SeqCst), 3);
}

#[test]
fn wayback_failure_is_recorded_and_local_copy_is_kept() {
    let s = site();
    let spn = Server::start(|_m, _url| Reply::status(429));
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let page = save_page(
        &store,
        &format!("{}/blog/post", s.base),
        &CaptureOptions::default(),
    )
    .unwrap();

    let after = backup_wayback(&store, page.id, &fast_wayback(&spn.base)).unwrap();
    assert_eq!(after.wayback_status, "failed");
    assert!(
        after
            .wayback_error
            .as_deref()
            .unwrap()
            .contains("rate-limiting"),
        "{:?}",
        after.wayback_error
    );
    assert!(after.wayback_url.is_none());
    assert!(store.file_path(&after).is_some(), "local copy untouched");

    // Error page without a job id.
    let spn2 = Server::start(|_m, _url| {
        Reply::ok(
            "text/html",
            "<html><p>This host has been excluded from the Wayback Machine.</p></html>",
        )
    });
    let err = fast_wayback(&spn2.base)
        .save("https://example.org/c")
        .unwrap_err();
    assert!(err.to_string().contains("excluded"), "{err}");
}

#[test]
fn wayback_daily_limit_links_the_latest_snapshot() {
    let spn = Server::start(|_m, url| {
        if url.starts_with("/wayback/available") {
            assert!(url.contains("url=https%3A%2F%2Fexample.org%2Fd"), "{url}");
            Reply::ok(
                "application/json",
                r#"{"url":"https://example.org/d","archived_snapshots":{"closest":{"status":"200","available":true,"url":"http://web.archive.org/web/20261123000000/https://example.org/d","timestamp":"20261123000000"}}}"#,
            )
        } else {
            Reply { status: 429, headers: vec![("Content-Type".into(), "text/html".into())], body: b"<h2>Sorry</h2><p>This URL has been already captured 5 times today, which is a daily limit. Please email us</p>".to_vec() }
        }
    });
    let url = fast_wayback(&spn.base)
        .save("https://example.org/d")
        .unwrap();
    assert_eq!(
        url,
        "https://web.archive.org/web/20261123000000/https://example.org/d"
    );
}

#[test]
fn ipfs_kubo_pins_and_records_cid() {
    let s = site();
    let kubo = Server::start(|m, url| {
        if m == "POST" && url.starts_with("/api/v0/add") {
            Reply::ok(
                "application/json",
                r#"{"Name":"x.html","Hash":"bafkreitestcid","Size":"12"}"#,
            )
        } else if url.starts_with("/api/v0/version") {
            Reply::ok("application/json", r#"{"Version":"0.38.1"}"#)
        } else {
            Reply::status(404)
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut st = settings::load(dir.path());
    st.ipfs.mode = IpfsMode::Kubo;
    st.ipfs.kubo_api_url = kubo.base.clone();
    settings::save(dir.path(), &st).unwrap();

    assert_eq!(
        ipfs::test_connection(&st.ipfs, &Secrets::default(), &ipfs::Endpoints::default()).unwrap(),
        "Connected: IPFS 0.38.1"
    );

    let page = save_page(
        &store,
        &format!("{}/blog/post", s.base),
        &CaptureOptions::default(),
    )
    .unwrap();
    let after = backup_ipfs(&store, page.id).unwrap();
    assert_eq!(after.ipfs_status, "done");
    assert_eq!(after.cid.as_deref(), Some("bafkreitestcid"));
    assert_eq!(after.pin_service.as_deref(), Some("kubo"));
    let adds = kubo.requests_to("/api/v0/add");
    assert_eq!(adds.len(), 1);
    assert!(adds[0].url.contains("pin=true"));
    assert!(
        String::from_utf8_lossy(&adds[0].body).contains("Quiet rivers"),
        "uploads the saved file"
    );
}

#[test]
fn ipfs_node_down_marks_failed_but_keeps_page() {
    let s = site();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut st = settings::load(dir.path());
    st.ipfs.mode = IpfsMode::Kubo;
    st.ipfs.kubo_api_url = "http://127.0.0.1:9".into(); // nothing listens here
    settings::save(dir.path(), &st).unwrap();
    let page = save_page(
        &store,
        &format!("{}/blog/post", s.base),
        &CaptureOptions::default(),
    )
    .unwrap();
    let after = backup_ipfs(&store, page.id).unwrap();
    assert_eq!(after.ipfs_status, "failed");
    assert!(after
        .ipfs_error
        .as_deref()
        .unwrap()
        .contains("no IPFS node answered"));
    assert!(store.file_path(&after).is_some());
}

#[test]
fn ipfs_pinata_uses_the_users_own_key() {
    let pinata = Server::start(|m, url| match (m, url) {
        ("POST", "/v3/files") => {
            Reply::ok("application/json", r#"{"data":{"cid":"bafypinatacid"}}"#)
        }
        ("GET", "/data/testAuthentication") => Reply::ok("application/json", r#"{"message":"ok"}"#),
        _ => Reply::status(404),
    });
    let ep = ipfs::Endpoints {
        pinata_upload: format!("{}/v3/files", pinata.base),
        pinata_auth: format!("{}/data/testAuthentication", pinata.base),
    };
    let mut st = settings::Settings::default();
    st.ipfs.mode = IpfsMode::Pinata;
    let none = Secrets::default();
    assert!(!ipfs::is_configured(&st.ipfs, &none));
    assert!(ipfs::test_connection(&st.ipfs, &none, &ep).is_err());

    let secrets = Secrets {
        pinata_jwt: Some("user-own-jwt-9876".into()),
        kubo_token: None,
    };
    assert!(ipfs::is_configured(&st.ipfs, &secrets));
    ipfs::test_connection(&st.ipfs, &secrets, &ep).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("p.html");
    std::fs::write(&f, "<p>hi</p>").unwrap();
    let pinned = ipfs::pin_file(&st.ipfs, &secrets, &ep, &f, "p.html").unwrap();
    assert_eq!(pinned.cid, "bafypinatacid");
    let up = &pinata.requests_to("/v3/files")[0];
    let auth = up
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .unwrap();
    assert_eq!(auth.1, "Bearer user-own-jwt-9876");
    assert!(String::from_utf8_lossy(&up.body).contains("public"));
}
