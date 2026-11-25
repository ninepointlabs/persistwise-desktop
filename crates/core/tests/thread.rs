//! Saving whole multi-page threads, against a local test server.

mod common;

use common::{Reply, Server};
use persistwise_core::save_page;
use persistwise_core::store::Store;
use persistwise_core::thread::{save_thread, ThreadOptions, ThreadSave};
use std::time::Duration;

fn opts() -> ThreadOptions {
    ThreadOptions {
        delay: Duration::ZERO,
        ..Default::default()
    }
}

fn page(title: &str, body: &str, head: &str) -> String {
    format!("<!doctype html><html><head><title>{title}</title>{head}</head><body><p>{body}</p></body></html>")
}

fn forum() -> Server {
    Server::start(|_m, url| {
        let (path, query) = url.split_once('?').unwrap_or((url, ""));
        let n: usize = query
            .strip_prefix("page=")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        match path {
            // Three pages chained with <link rel="next"> / <a rel="next">.
            "/t/old-radios" => {
                let next = match n {
                    1 => r#"<link rel="next" href="/t/old-radios?page=2">"#.to_string(),
                    2 => r#"<a rel="next" href="/t/old-radios?page=3">Next</a>"#.to_string(),
                    _ => String::new(),
                };
                let body = match n {
                    1 => "Valve radios hum when they warm up.",
                    2 => "The zanzibar crystal set needs no battery.",
                    _ => "Last page: thanks everyone.",
                };
                Reply::ok(
                    "text/html",
                    page(&format!("Old radios - page {n}"), body, &next),
                )
            }
            // Pagination only in Link: headers.
            "/h" => {
                let r = Reply::ok(
                    "text/html",
                    page(&format!("Header page {n}"), &format!("header body {n}"), ""),
                );
                if n < 3 {
                    r.header("Link", &format!("</h?page={}>; rel=\"next\"", n + 1))
                } else {
                    r
                }
            }
            // Page 2 points back at page 1.
            "/loop" => {
                let next = if n == 1 { "/loop?page=2" } else { "/loop" };
                Reply::ok(
                    "text/html",
                    page(
                        "Loop",
                        "round and round",
                        &format!(r#"<link rel="next" href="{next}">"#),
                    ),
                )
            }
            // Endless pages, for the cap.
            "/endless" => Reply::ok(
                "text/html",
                page(
                    "Endless",
                    &format!("page {n}"),
                    &format!(r#"<link rel="next" href="/endless?page={}">"#, n + 1),
                ),
            ),
            // Page 3 fails.
            "/flaky" => {
                if n >= 3 {
                    Reply::status(500)
                } else {
                    Reply::ok(
                        "text/html",
                        page(
                            "Flaky",
                            &format!("flaky {n}"),
                            &format!(r#"<link rel="next" href="/flaky?page={}">"#, n + 1),
                        ),
                    )
                }
            }
            "/single" => Reply::ok("text/html", page("Just one", "a lonely page", "")),
            _ => Reply::status(404),
        }
    })
}

fn save(store: &Store, url: &str, o: &ThreadOptions) -> (ThreadSave, Vec<usize>) {
    let mut progress = Vec::new();
    let r = save_thread(store, url, o, &mut |p| progress.push(p.page)).unwrap();
    (r, progress)
}

#[test]
fn rel_next_thread_is_saved_as_one_group_in_order() {
    let s = forum();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (r, progress) = save(&store, &format!("{}/t/old-radios", s.base), &opts());

    assert!(!r.single && !r.partial, "{}", r.message);
    assert_eq!(progress, vec![1, 2, 3]);
    let t = r.thread.expect("a thread");
    assert_eq!(t.status, "complete");
    assert_eq!(t.title.as_deref(), Some("Old radios - page 1"));
    let titles: Vec<_> = t.pages.iter().map(|p| p.title.clone().unwrap()).collect();
    assert_eq!(
        titles,
        [
            "Old radios - page 1",
            "Old radios - page 2",
            "Old radios - page 3"
        ]
    );
    for (i, p) in t.pages.iter().enumerate() {
        assert_eq!(p.thread_id, Some(t.id));
        assert_eq!(p.thread_page, Some(i as i64 + 1));
        assert_eq!(p.thread_page_count, Some(3));
        let file = store.file_path(p).expect("saved file");
        assert!(std::fs::read_to_string(file).unwrap().contains("<p>"));
    }
    assert!(t.pages[2].url.ends_with("/t/old-radios?page=3"));
    // Every page is in the search index.
    for word in ["valve", "zanzibar", "thanks"] {
        assert_eq!(
            store.list_pages(Some(word), None, 10, 0).unwrap().len(),
            1,
            "{word}"
        );
    }
    // The library shows one item for the thread.
    let lib = store.list_pages(None, None, 50, 0).unwrap();
    assert_eq!(lib.len(), 1);
    assert_eq!(lib[0].thread_page_count, Some(3));
    assert_eq!(lib[0].thread_page, Some(1));
}

#[test]
fn link_headers_are_followed() {
    let s = forum();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (r, _) = save(&store, &format!("{}/h", s.base), &opts());
    let t = r.thread.unwrap();
    assert_eq!(t.pages.len(), 3);
    assert!(t.pages[1].url.ends_with("/h?page=2"));
    assert!(t.pages[2].url.ends_with("/h?page=3"));
}

#[test]
fn loops_stop_and_offsite_next_is_ignored() {
    let s = forum();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (r, _) = save(&store, &format!("{}/loop", s.base), &opts());
    assert!(!r.partial);
    assert_eq!(r.thread.unwrap().pages.len(), 2);
    // Each page fetched once.
    assert_eq!(s.requests_to("/loop").len(), 2);

    // A next link to another host is not followed.
    let port = s.base.rsplit(':').next().unwrap().to_string();
    let other = Server::start(move |_m, url| match url {
        "/start" => Reply::ok(
            "text/html",
            page(
                "Start",
                "first",
                &format!(r#"<link rel="next" href="http://localhost:{port}/single">"#),
            ),
        ),
        _ => Reply::status(404),
    });
    let (r, _) = save(&store, &format!("{}/start", other.base), &opts());
    assert!(r.single, "{}", r.message);
    assert!(s.requests_to("/single").is_empty());
}

#[test]
fn page_cap_stops_with_a_partial_thread() {
    let s = forum();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let o = ThreadOptions {
        max_pages: 4,
        ..opts()
    };
    let (r, _) = save(&store, &format!("{}/endless", s.base), &o);
    assert!(r.partial);
    assert!(r.message.contains("4-page limit"), "{}", r.message);
    let t = r.thread.unwrap();
    assert_eq!(t.status, "partial");
    assert!(t.note.unwrap().contains("limit"));
    assert_eq!(t.pages.len(), 4);
    assert_eq!(store.stats().unwrap().pages, 4);
    assert_eq!(s.requests_to("/endless").len(), 4);
}

#[test]
fn a_failed_page_keeps_the_earlier_ones() {
    let s = forum();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (r, _) = save(&store, &format!("{}/flaky", s.base), &opts());
    assert!(r.partial);
    assert!(r.message.contains("page 3"), "{}", r.message);
    let t = r.thread.unwrap();
    assert_eq!(t.status, "partial");
    assert!(t.note.unwrap().contains("HTTP 500"));
    assert_eq!(t.pages.len(), 2);
    for p in &t.pages {
        assert!(store.file_path(p).is_some());
    }
}

#[test]
fn a_single_page_saves_without_error() {
    let s = forum();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (r, _) = save(&store, &format!("{}/single", s.base), &opts());
    assert!(r.single && !r.partial);
    assert!(r.thread.is_none());
    assert!(r.message.contains("did not look like a multi-page thread"));
    assert_eq!(r.pages.len(), 1);
    assert_eq!(r.pages[0].thread_id, None);
    assert_eq!(store.stats().unwrap().pages, 1);
}

#[test]
fn the_first_page_failing_is_an_error() {
    let s = forum();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert!(save_thread(&store, &format!("{}/nope", s.base), &opts(), &mut |_| {}).is_err());
    assert_eq!(store.stats().unwrap().pages, 0);
}

#[test]
fn search_points_at_the_right_page() {
    let s = forum();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (r, _) = save(&store, &format!("{}/t/old-radios", s.base), &opts());
    let t = r.thread.unwrap();
    let hits = store.list_pages(Some("zanzibar"), None, 10, 0).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, t.pages[1].id);
    assert_eq!(hits[0].thread_id, Some(t.id));
    assert_eq!(hits[0].thread_page, Some(2));
    assert!(hits[0].snippet.as_deref().unwrap().contains("zanzibar"));
}

#[test]
fn saving_again_reuses_pages() {
    let s = forum();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    // Page 2 was saved on its own before.
    let earlier = save_page(
        &store,
        &format!("{}/t/old-radios?page=2", s.base),
        &Default::default(),
    )
    .unwrap();

    let (first, _) = save(&store, &format!("{}/t/old-radios", s.base), &opts());
    assert_eq!(first.reused, 1);
    assert_eq!(first.new_page_ids.len(), 2);
    let t1 = first.thread.unwrap();
    assert_eq!(t1.pages[1].id, earlier.id);
    assert_eq!(store.stats().unwrap().pages, 3);

    let (again, _) = save(&store, &format!("{}/t/old-radios", s.base), &opts());
    assert_eq!(again.reused, 3);
    assert!(again.new_page_ids.is_empty());
    let t2 = again.thread.unwrap();
    assert_eq!(t2.id, t1.id);
    assert_eq!(t2.pages.len(), 3);
    assert_eq!(store.stats().unwrap().pages, 3);
    assert_eq!(store.list_pages(None, None, 50, 0).unwrap().len(), 1);
}

#[test]
fn deleting_pages_and_threads() {
    let s = forum();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (r, _) = save(&store, &format!("{}/t/old-radios", s.base), &opts());
    let t = r.thread.unwrap();
    store.delete_page(t.pages[0].id).unwrap();
    let t = store.thread(t.id).unwrap().unwrap();
    assert_eq!(t.pages.len(), 2);
    assert_eq!(t.pages[0].thread_page, Some(1));
    // The library now shows the thread through its new first page.
    let lib = store.list_pages(None, None, 50, 0).unwrap();
    assert_eq!(lib.len(), 1);
    assert_eq!(lib[0].id, t.pages[0].id);
    store.delete_thread(t.id).unwrap();
    assert!(store.thread(t.id).unwrap().is_none());
    assert_eq!(store.stats().unwrap().pages, 0);
}
