mod common;

use common::{Reply, Server, PNG};
use persistwise_core::capture::CaptureOptions;
use persistwise_core::store::Store;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// An export whose saved copy links its image and stylesheet on `base`,
/// like the Persistwise website stored pages.
fn export_dir(base: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("saved-copies")).unwrap();
    let html = format!(
        "<html><head><title>Imported</title><link rel=\"stylesheet\" href=\"{base}/s.css\"><script>x()</script></head>\
         <body><p>Imported words</p><img src=\"{base}/pic.png\"></body></html>"
    );
    std::fs::write(dir.path().join("saved-copies/1.html"), &html).unwrap();
    let m = serde_json::json!({
        "format": "persistwise-export", "version": 1, "source": {"host": "web.test"},
        "feeds": [{"id": 1, "url": format!("{base}/feed.xml"), "name": "F"}],
        "posts": [{"id": 1, "feed_id": 1, "guid": "g1", "url": format!("{base}/post"), "title": "Imported",
                   "fetched_at": "2026-03-01T00:00:00Z",
                   "saved_copy": {"path": "saved-copies/1.html", "sha256": persistwise_core::util::sha256_hex(html.as_bytes())}}]
    });
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_vec(&m).unwrap(),
    )
    .unwrap();
    dir
}

#[test]
fn imported_copies_get_their_images_embedded_once() {
    let up = Arc::new(AtomicBool::new(false));
    let up2 = up.clone();
    let s = Server::start(move |_m, url| {
        if !up2.load(Ordering::SeqCst) {
            return Reply::status(503);
        }
        match url {
            "/s.css" => Reply::ok("text/css", "body{background:url(bg.png)}"),
            "/pic.png" | "/bg.png" => Reply::ok("image/png", PNG.to_vec()),
            _ => Reply::status(404),
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let ex = export_dir(&s.base);
    persistwise_core::import::import(&store, ex.path()).unwrap();
    assert_eq!(store.pages_needing_localize(10).unwrap().len(), 1);

    // Site down: nothing fetched, page untouched, tried again later.
    let page = store.list_pages(None, None, 10, 0).unwrap()[0].clone();
    assert!(persistwise_core::localize_page(&store, page.id, &CaptureOptions::default()).is_err());
    assert_eq!(store.pages_needing_localize(10).unwrap().len(), 1);
    assert_eq!(store.page(page.id).unwrap().unwrap().sha256, page.sha256);

    up.store(true, Ordering::SeqCst);
    let n =
        persistwise_core::localize_imported(&store, 25, &CaptureOptions::default(), &mut |_| {});
    assert_eq!(n, 1);
    let after = store.page(page.id).unwrap().unwrap();
    assert!(after.localized_at.is_some());
    assert_eq!(after.saved_at, page.saved_at, "original save date kept");
    assert_eq!(after.assets_inlined, 3);
    let html = std::fs::read_to_string(store.file_path(&after).unwrap()).unwrap();
    assert!(html.contains("data:image/png;base64,"));
    assert!(!html.contains(&format!("{}/pic.png", s.base)));
    assert!(!html.contains("<script"));
    assert!(store.pages_needing_localize(10).unwrap().is_empty());

    // A later re-import with the same web copy does not undo it.
    let r = persistwise_core::import::import(&store, ex.path()).unwrap();
    assert_eq!(r.copies_saved, 0, "{r:?}");
    let still = store.page(page.id).unwrap().unwrap();
    assert_eq!(still.sha256, after.sha256);
}
