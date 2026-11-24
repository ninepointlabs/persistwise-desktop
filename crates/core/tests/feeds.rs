mod common;

use common::{Reply, Server};
use persistwise_core::capture::CaptureOptions;
use persistwise_core::feeds::{self, AlreadyPolling, PollOptions};
use persistwise_core::store::Store;
use persistwise_core::wayback::Wayback;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Site {
    server: Server,
    feed_xml: Arc<Mutex<String>>,
    post2_up: Arc<AtomicBool>,
    feed_up: Arc<AtomicBool>,
}

fn rss(base: &str, items: &[(&str, &str)]) -> String {
    let items: String = items
        .iter()
        .map(|(slug, title)| {
            format!(
                "<item><title>{title}</title><link>{base}/posts/{slug}</link><guid>{base}/posts/{slug}</guid>\
                 <author>a@example.com (A. Writer)</author><pubDate>Mon, 02 Nov 2026 10:00:00 GMT</pubDate></item>"
            )
        })
        .collect();
    format!("<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>Tide Notes</title><link>{base}/</link>{items}</channel></rss>")
}

fn site() -> Site {
    let feed_xml = Arc::new(Mutex::new(String::new()));
    let post2_up = Arc::new(AtomicBool::new(true));
    let feed_up = Arc::new(AtomicBool::new(true));
    let (fx, p2, fu) = (feed_xml.clone(), post2_up.clone(), feed_up.clone());
    let server = Server::start(move |_m, url| match url {
        "/" => Reply::ok(
            "text/html",
            r#"<html><head><title>Tide Notes</title><link rel="alternate" type="application/rss+xml" href="/feed.xml"></head><body>home</body></html>"#,
        ),
        "/feed.xml" if fu.load(Ordering::SeqCst) => {
            Reply::ok("application/rss+xml", fx.lock().unwrap().clone())
        }
        "/feed.xml" => Reply::status(503),
        "/posts/two" if !p2.load(Ordering::SeqCst) => Reply::status(500),
        u if u.starts_with("/posts/") => {
            let slug = &u["/posts/".len()..];
            Reply::ok("text/html", format!("<html><head><title>Post {slug}</title></head><body><p>Body of the {slug} post about tides.</p></body></html>"))
        }
        u if u.starts_with("/save/") => Reply::status(302).header(
            "Location",
            &format!("https://web.archive.org/web/20261123000000/{}", &u[6..]),
        ),
        _ => Reply::status(404),
    });
    *feed_xml.lock().unwrap() = rss(&server.base, &[("two", "Second"), ("one", "First")]);
    Site {
        server,
        feed_xml,
        post2_up,
        feed_up,
    }
}

fn opts(base: &str, wayback: bool) -> PollOptions {
    PollOptions {
        capture: CaptureOptions::default(),
        wayback: wayback.then(|| Wayback {
            base: base.into(),
            availability_base: base.into(),
            poll_interval: Duration::from_millis(5),
            max_polls: 2,
            timeout: Duration::from_secs(5),
        }),
        ipfs: false,
        max_capture_attempts: 5,
        backup_retry_minutes: 55,
        wayback_spacing: Duration::from_millis(1),
    }
}

#[test]
fn subscribe_finds_the_feed_from_a_web_page() {
    let s = site();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (feed, created) = feeds::subscribe(&store, &format!("{}/", s.server.base)).unwrap();
    assert!(created);
    assert_eq!(feed.url, format!("{}/feed.xml", s.server.base));
    assert_eq!(feed.name.as_deref(), Some("Tide Notes"));
    assert_eq!(feed.origin, "desktop");
    // Subscribing again returns the same feed.
    let (again, created) =
        feeds::subscribe(&store, &format!("{}/feed.xml", s.server.base)).unwrap();
    assert!(!created);
    assert_eq!(again.id, feed.id);
    // Not a feed and no feed link.
    assert!(feeds::subscribe(&store, &format!("{}/posts/one", s.server.base)).is_err());
}

#[test]
fn polling_saves_new_entries_once_and_backs_them_up() {
    let s = site();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (feed, _) = feeds::subscribe(&store, &format!("{}/feed.xml", s.server.base)).unwrap();

    let mut changed = Vec::new();
    let r = &feeds::poll_all(&store, None, &opts(&s.server.base, true), &mut |id| {
        changed.push(id)
    })
    .unwrap()[0];
    assert_eq!(
        (r.new_pages, r.saved, r.capture_failed, r.wayback_done),
        (2, 2, 0, 2),
        "{r:?}"
    );
    assert!(!changed.is_empty());

    let pages = store.list_pages(None, Some(feed.id), 10, 0).unwrap();
    assert_eq!(pages.len(), 2);
    for p in &pages {
        assert_eq!(p.origin, "feed");
        assert_eq!(p.author.as_deref(), Some("A. Writer"));
        assert_eq!(p.published_at.as_deref(), Some("2026-11-02T10:00:00Z"));
        assert!(store.file_path(p).is_some());
        assert_eq!(p.wayback_status, "done");
        assert!(p
            .wayback_url
            .as_deref()
            .unwrap()
            .contains("/web/20261123000000/"));
    }
    assert_eq!(
        store.list_pages(Some("tides"), None, 10, 0).unwrap().len(),
        2,
        "feed pages are searchable"
    );
    let f = store.feed(feed.id).unwrap().unwrap();
    assert_eq!(f.status.as_deref(), Some("active"));
    assert!(f.last_polled_at.is_some());

    // Nothing new: nothing saved, nothing resubmitted.
    let spn_before = s.server.requests_to("/save/").len();
    let r2 = &feeds::poll_all(&store, None, &opts(&s.server.base, true), &mut |_| {}).unwrap()[0];
    assert_eq!((r2.new_pages, r2.saved, r2.wayback_done), (0, 0, 0));
    assert_eq!(s.server.requests_to("/save/").len(), spn_before);

    // A new post shows up.
    *s.feed_xml.lock().unwrap() = rss(
        &s.server.base,
        &[("three", "Third"), ("two", "Second"), ("one", "First")],
    );
    let r3 = &feeds::poll_all(&store, None, &opts(&s.server.base, true), &mut |_| {}).unwrap()[0];
    assert_eq!((r3.new_pages, r3.saved, r3.wayback_done), (1, 1, 1));
    assert_eq!(
        store.list_pages(None, Some(feed.id), 10, 0).unwrap().len(),
        3
    );
}

#[test]
fn failed_downloads_are_retried_and_wayback_off_is_respected() {
    let s = site();
    s.post2_up.store(false, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (feed, _) = feeds::subscribe(&store, &format!("{}/feed.xml", s.server.base)).unwrap();

    let r = &feeds::poll_all(&store, None, &opts(&s.server.base, false), &mut |_| {}).unwrap()[0];
    assert_eq!((r.new_pages, r.saved, r.capture_failed), (2, 1, 1));
    assert_eq!(
        s.server.requests_to("/save/").len(),
        0,
        "Wayback off: nothing sent"
    );
    let broken = store.list_pages(Some("Second"), None, 10, 0).unwrap();
    let broken = &broken[0];
    assert!(broken.local_path.is_none());
    assert!(broken.capture_error.as_deref().unwrap().contains("500"));

    s.post2_up.store(true, Ordering::SeqCst);
    let r2 = &feeds::poll_all(&store, None, &opts(&s.server.base, false), &mut |_| {}).unwrap()[0];
    assert_eq!((r2.new_pages, r2.saved, r2.capture_failed), (0, 1, 0));
    let fixed = store.page(broken.id).unwrap().unwrap();
    assert!(store.file_path(&fixed).is_some());
    assert!(fixed.capture_error.is_none());
    assert_eq!(
        store.list_pages(None, Some(feed.id), 10, 0).unwrap().len(),
        2
    );
}

#[test]
fn errors_pauses_and_the_lock() {
    let s = site();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (feed, _) = feeds::subscribe(&store, &format!("{}/feed.xml", s.server.base)).unwrap();

    s.feed_up.store(false, Ordering::SeqCst);
    let r = &feeds::poll_all(&store, None, &opts(&s.server.base, false), &mut |_| {}).unwrap()[0];
    assert!(r.error.as_deref().unwrap().contains("503"));
    let f = store.feed(feed.id).unwrap().unwrap();
    assert_eq!(f.status.as_deref(), Some("errored"));
    assert!(f.last_error.is_some());

    // Errored feeds are retried and recover.
    s.feed_up.store(true, Ordering::SeqCst);
    feeds::poll_all(&store, None, &opts(&s.server.base, false), &mut |_| {}).unwrap();
    let f = store.feed(feed.id).unwrap().unwrap();
    assert_eq!(f.status.as_deref(), Some("active"));
    assert!(f.last_error.is_none());

    // Paused feeds are skipped by the scheduled check, but "check now" on
    // that one feed still works.
    store
        .set_feed_status(feed.id, "paused", None, false)
        .unwrap();
    assert!(
        feeds::poll_all(&store, None, &opts(&s.server.base, false), &mut |_| {})
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        feeds::poll_all(
            &store,
            Some(feed.id),
            &opts(&s.server.base, false),
            &mut |_| {}
        )
        .unwrap()
        .len(),
        1
    );

    // Only one poller at a time.
    let lock = std::fs::File::create(dir.path().join("poll.lock")).unwrap();
    lock.lock().unwrap();
    let err = feeds::poll_all(&store, None, &opts(&s.server.base, false), &mut |_| {}).unwrap_err();
    assert!(err.downcast_ref::<AlreadyPolling>().is_some());
    drop(lock);

    // Removing a feed keeps its pages.
    store.delete_feed(feed.id).unwrap();
    assert!(store.feed(feed.id).unwrap().is_none());
    assert_eq!(store.list_pages(None, None, 10, 0).unwrap().len(), 2);
}

#[test]
fn a_later_web_import_adopts_polled_feeds_and_keeps_local_copies() {
    let s = site();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    feeds::subscribe(&store, &format!("{}/feed.xml", s.server.base)).unwrap();
    feeds::poll_all(&store, None, &opts(&s.server.base, false), &mut |_| {}).unwrap();
    let local = store.list_pages(Some("First"), None, 10, 0).unwrap()[0].clone();

    // An export of the same feed from the web app.
    let ex = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(ex.path().join("saved-copies")).unwrap();
    let web_html = b"<html><body>web copy</body></html>";
    std::fs::write(ex.path().join("saved-copies/9.html"), web_html).unwrap();
    let manifest = serde_json::json!({
        "format": "persistwise-export", "version": 1, "source": {"host": "persistwise.example"},
        "feeds": [{"id": 3, "url": format!("{}/feed.xml", s.server.base), "name": "Tide Notes", "status": "active"}],
        "posts": [{"id": 9, "feed_id": 3, "guid": format!("{}/posts/one", s.server.base), "url": format!("{}/posts/one", s.server.base),
                   "title": "First", "archive_record": {"id": 9, "wayback_url": "https://web.archive.org/web/1/x", "cid": null},
                   "saved_copy": {"path": "saved-copies/9.html", "sha256": persistwise_core::util::sha256_hex(web_html)}}]
    });
    std::fs::write(
        ex.path().join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();

    let r = persistwise_core::import::import(&store, ex.path()).unwrap();
    assert_eq!(
        (
            r.feeds_created,
            r.feeds_updated,
            r.pages_created,
            r.pages_updated
        ),
        (0, 1, 0, 1),
        "{r:?}"
    );
    assert_eq!(store.feeds().unwrap().len(), 1);
    assert_eq!(store.list_pages(None, None, 10, 0).unwrap().len(), 2);
    let after = store.page(local.id).unwrap().unwrap();
    assert_eq!(
        after.sha256, local.sha256,
        "the locally captured copy is kept"
    );
    assert_eq!(
        after.wayback_status, "done",
        "the web app's Wayback link is merged in"
    );
}
