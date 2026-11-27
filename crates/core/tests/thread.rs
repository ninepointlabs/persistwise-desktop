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

/// A thread that grows between saves: `total` pages; page 2 carries
/// `replies` (it is the last page after the first save, where new replies
/// land); page `fail` (if not 0) answers 500.
struct Growing {
    server: Server,
    total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    replies: std::sync::Arc<std::sync::Mutex<String>>,
    fail: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

fn growing(total: usize) -> Growing {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    let t = Arc::new(AtomicUsize::new(total));
    let replies = Arc::new(Mutex::new(String::from("first reply")));
    let fail = Arc::new(AtomicUsize::new(0));
    let (t2, r2, f2) = (t.clone(), replies.clone(), fail.clone());
    let server = Server::start(move |_m, url| {
        let (path, query) = url.split_once('?').unwrap_or((url, ""));
        if path != "/grow" {
            return Reply::status(404);
        }
        let n: usize = query
            .strip_prefix("page=")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        let total = t2.load(Ordering::SeqCst);
        if n > total {
            return Reply::status(404);
        }
        if n == f2.load(Ordering::SeqCst) {
            return Reply::status(500);
        }
        let next = if n < total {
            format!(r#"<link rel="next" href="/grow?page={}">"#, n + 1)
        } else {
            String::new()
        };
        let body = if n == 2 {
            format!("page {n} of the grow thread; {}", r2.lock().unwrap())
        } else {
            format!("page {n} of the grow thread")
        };
        Reply::ok("text/html", page(&format!("Grow {n}"), &body, &next))
    });
    Growing {
        server,
        total: t,
        replies,
        fail,
    }
}

impl Growing {
    fn set_total(&self, n: usize) {
        self.total.store(n, std::sync::atomic::Ordering::SeqCst);
    }
    fn set_replies(&self, s: &str) {
        *self.replies.lock().unwrap() = s.to_string();
    }
    fn set_fail(&self, n: usize) {
        self.fail.store(n, std::sync::atomic::Ordering::SeqCst);
    }
    /// Requests for exactly this path and query.
    fn hits(&self, path_and_query: &str) -> usize {
        self.server
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.url == path_and_query)
            .count()
    }
    fn url(&self) -> String {
        format!("{}/grow", self.server.base)
    }
}

#[test]
fn resave_refreshes_the_last_page_and_appends_new_ones() {
    let g = growing(2);
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (first, _) = save(&store, &g.url(), &opts());
    let t1 = first.thread.unwrap();
    assert_eq!(t1.pages.len(), 2);
    let (p1, p2) = (t1.pages[0].clone(), t1.pages[1].clone());
    assert_eq!((g.hits("/grow"), g.hits("/grow?page=2")), (1, 1));

    // Two more pages appear, and page 2 gets a new reply.
    g.set_total(4);
    g.set_replies("a kiwifruit reply");
    let (again, progress) = save(&store, &g.url(), &opts());
    assert!(
        again.resumed && !again.partial && !again.single,
        "{}",
        again.message
    );
    // Page 1 was reused without being fetched again.
    assert_eq!(g.hits("/grow"), 1);
    assert_eq!(g.hits("/grow?page=2"), 2);
    assert_eq!((g.hits("/grow?page=3"), g.hits("/grow?page=4")), (1, 1));
    assert_eq!(progress, vec![2, 3, 4]);
    assert_eq!(again.reused, 1);
    assert_eq!(again.refreshed_page_ids, vec![p2.id]);
    assert_eq!(again.new_page_ids.len(), 2);
    assert!(
        again.message.contains("2 new pages added"),
        "{}",
        again.message
    );

    // One continuous thread, in order, with no duplicates.
    let t2 = again.thread.unwrap();
    assert_eq!(t2.id, t1.id);
    let urls: Vec<_> = t2.pages.iter().map(|p| p.url.clone()).collect();
    assert_eq!(
        urls,
        [
            g.url(),
            format!("{}?page=2", g.url()),
            format!("{}?page=3", g.url()),
            format!("{}?page=4", g.url()),
        ]
    );
    assert_eq!(t2.pages[0], store.page(p1.id).unwrap().unwrap());
    assert_eq!(t2.pages[1].id, p2.id);
    for (i, p) in t2.pages.iter().enumerate() {
        assert_eq!(p.thread_page, Some(i as i64 + 1));
        assert_eq!(p.thread_page_count, Some(4));
    }
    assert_eq!(store.stats().unwrap().pages, 4);
    assert_eq!(store.list_pages(None, None, 50, 0).unwrap().len(), 1);

    // The refreshed page has the new reply, in its copy and in search.
    let hits = store.list_pages(Some("kiwifruit"), None, 10, 0).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, p2.id);
    let copy = std::fs::read_to_string(store.file_path(&t2.pages[1]).unwrap()).unwrap();
    assert!(copy.contains("kiwifruit"));
    assert!(store
        .list_pages(Some("first"), None, 10, 0)
        .unwrap()
        .is_empty());

    // Nothing new: only the last page is checked, and it is left alone.
    let (third, _) = save(&store, &g.url(), &opts());
    assert!(third.resumed && third.new_page_ids.is_empty() && third.refreshed_page_ids.is_empty());
    assert_eq!(third.reused, 4);
    assert!(third.message.contains("none found"), "{}", third.message);
    assert_eq!((g.hits("/grow?page=2"), g.hits("/grow?page=4")), (2, 2));
    assert_eq!(store.stats().unwrap().pages, 4);
}

#[test]
fn resave_from_a_later_page_continues_the_same_thread() {
    let g = growing(3);
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let t1 = save(&store, &g.url(), &opts()).0.thread.unwrap();
    g.set_total(4);
    let (r, _) = save(&store, &format!("{}?page=2", g.url()), &opts());
    let t2 = r.thread.unwrap();
    assert_eq!(t2.id, t1.id);
    assert_eq!(t2.pages.len(), 4);
    assert_eq!(g.hits("/grow"), 1);
    assert_eq!(g.hits("/grow?page=2"), 1);
}

#[test]
fn resave_failure_while_appending_is_partial_and_keeps_pages() {
    let g = growing(2);
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let t1 = save(&store, &g.url(), &opts()).0.thread.unwrap();

    g.set_total(4);
    g.set_fail(4);
    let (r, _) = save(&store, &g.url(), &opts());
    assert!(r.partial && r.resumed);
    assert!(r.message.contains("page 4"), "{}", r.message);
    let t = r.thread.unwrap();
    assert_eq!(
        (t.id, t.status.as_str(), t.pages.len()),
        (t1.id, "partial", 3)
    );
    assert!(t.note.unwrap().contains("HTTP 500"));

    // Once the site recovers, the next re-save finishes the thread.
    g.set_fail(0);
    let (r, _) = save(&store, &g.url(), &opts());
    let t = r.thread.unwrap();
    assert_eq!(
        (t.id, t.status.as_str(), t.pages.len()),
        (t1.id, "complete", 4)
    );
    assert_eq!(store.stats().unwrap().pages, 4);
}

#[test]
fn resave_respects_the_page_cap() {
    let g = growing(2);
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    save(&store, &g.url(), &opts());
    g.set_total(6);
    let o = ThreadOptions {
        max_pages: 3,
        ..opts()
    };
    let (r, _) = save(&store, &g.url(), &o);
    assert!(r.partial);
    assert!(r.message.contains("3-page limit"), "{}", r.message);
    let t = r.thread.unwrap();
    assert_eq!((t.status.as_str(), t.pages.len()), ("partial", 3));
    assert_eq!(g.hits("/grow?page=4"), 0);
}

#[test]
fn resave_when_the_last_page_cannot_be_fetched_changes_nothing() {
    let g = growing(2);
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let t1 = save(&store, &g.url(), &opts()).0.thread.unwrap();
    g.set_fail(2);
    let err = save_thread(&store, &g.url(), &opts(), &mut |_| {})
        .unwrap_err()
        .to_string();
    assert!(err.contains("unchanged"), "{err}");
    let t = store.thread(t1.id).unwrap().unwrap();
    assert_eq!(t, t1);
}

/// A Kubo RPC stand-in: `pin/rm` answers with `rm` (status, body).
fn kubo(rm: (u16, &'static str)) -> Server {
    Server::start(move |m, url| {
        if m == "POST" && url.starts_with("/api/v0/pin/rm") {
            let mut r = Reply::ok("application/json", rm.1);
            r.status = rm.0;
            r
        } else {
            Reply::status(404)
        }
    })
}

fn use_kubo(dir: &std::path::Path, base: &str) {
    use persistwise_core::settings::{self, IpfsMode};
    let mut st = settings::load(dir);
    st.ipfs.mode = IpfsMode::Kubo;
    st.ipfs.kubo_api_url = base.to_string();
    settings::save(dir, &st).unwrap();
}

#[test]
fn resave_removes_the_old_ipfs_pin_of_a_replaced_page() {
    let g = growing(2);
    let node = kubo((200, r#"{"Pins":["bafyoldcopy"]}"#));
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    use_kubo(dir.path(), &node.base);
    let (first, _) = save(&store, &g.url(), &opts());
    let t = first.thread.unwrap();
    let (p1, p2) = (t.pages[0].id, t.pages[1].id);
    store
        .set_ipfs(p1, "done", Some("bafyfirst"), Some("kubo"), None)
        .unwrap();
    store
        .set_ipfs(p2, "done", Some("bafyoldcopy"), Some("kubo"), None)
        .unwrap();

    // Unchanged (saved again in a later second, so only the copy's
    // saved-at stamp differs): not refreshed, nothing is unpinned.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (same, _) = save(&store, &g.url(), &opts());
    assert!(same.refreshed_page_ids.is_empty(), "{}", same.message);
    assert!(same.unpinned.is_empty());
    assert!(node.requests_to("/api/v0/pin/rm").is_empty());

    // Page 2 changes: its old pin goes, page 1's stays.
    g.set_replies("a new reply");
    let (again, _) = save(&store, &g.url(), &opts());
    assert_eq!(again.refreshed_page_ids, vec![p2]);
    assert_eq!(again.unpinned, vec!["bafyoldcopy".to_string()]);
    let rm = node.requests_to("/api/v0/pin/rm");
    assert_eq!(rm.len(), 1);
    assert_eq!(rm[0].url, "/api/v0/pin/rm?arg=bafyoldcopy");
    assert!(
        again.message.contains("earlier copy was removed"),
        "{}",
        again.message
    );
    let p2 = store.page(p2).unwrap().unwrap();
    assert!(p2.cid.is_none() && p2.pin_service.is_none());
    assert_eq!(
        store.page(p1).unwrap().unwrap().cid.as_deref(),
        Some("bafyfirst")
    );
}

#[test]
fn resave_keeps_a_pin_another_page_still_uses() {
    let g = growing(2);
    let node = kubo((200, "{}"));
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    use_kubo(dir.path(), &node.base);
    let (first, _) = save(&store, &g.url(), &opts());
    let t = first.thread.unwrap();
    for p in &t.pages {
        store
            .set_ipfs(p.id, "done", Some("bafyshared"), Some("kubo"), None)
            .unwrap();
    }
    g.set_replies("another reply");
    let (again, _) = save(&store, &g.url(), &opts());
    assert_eq!(again.refreshed_page_ids.len(), 1);
    assert!(again.unpinned.is_empty());
    assert!(node.requests_to("/api/v0/pin/rm").is_empty());
    assert!(!again.message.contains("IPFS"), "{}", again.message);
}

#[test]
fn resave_reports_an_old_pin_it_could_not_remove() {
    let g = growing(2);
    let node = kubo((500, r#"{"Message":"permission denied","Type":"error"}"#));
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    use_kubo(dir.path(), &node.base);
    let (first, _) = save(&store, &g.url(), &opts());
    let p2 = first.thread.unwrap().pages[1].id;
    store
        .set_ipfs(p2, "done", Some("bafystuck"), Some("kubo"), None)
        .unwrap();
    g.set_replies("yet another reply");
    let (again, _) = save(&store, &g.url(), &opts());
    // The save itself succeeds; the message names the pin left behind.
    assert_eq!(again.refreshed_page_ids, vec![p2]);
    assert!(again.unpinned.is_empty());
    assert!(
        again.message.contains("(bafystuck) could not be removed")
            && again.message.contains("HTTP 500"),
        "{}",
        again.message
    );

    // Pinned with a service that is no longer set up: reported, not attempted.
    store
        .set_ipfs(p2, "done", Some("bafypinata"), Some("pinata"), None)
        .unwrap();
    g.set_replies("and one more");
    let before = node.requests_to("/api/v0/pin/rm").len();
    let (third, _) = save(&store, &g.url(), &opts());
    assert_eq!(node.requests_to("/api/v0/pin/rm").len(), before);
    assert!(
        third.message.contains("(bafypinata) could not be removed")
            && third.message.contains("pinned with Pinata"),
        "{}",
        third.message
    );
}
