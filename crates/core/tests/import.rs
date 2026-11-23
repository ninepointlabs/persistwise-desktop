use persistwise_core::import::{import, read_manifest};
use persistwise_core::store::Store;
use std::io::Write;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/export-v1")
}

/// Zips the fixture folder, optionally under a top-level folder and with
/// extra (hostile) entries.
fn zip_fixture(dest: &Path, prefix: &str, extra: &[(&str, &[u8])]) -> PathBuf {
    let out = dest.join("export.zip");
    let mut zw = zip::ZipWriter::new(std::fs::File::create(&out).unwrap());
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for rel in ["manifest.json", "saved-copies/101.html", "saved-copies/102.html"] {
        zw.start_file(format!("{prefix}{rel}"), opts).unwrap();
        zw.write_all(&std::fs::read(fixture().join(rel)).unwrap()).unwrap();
    }
    for (name, bytes) in extra {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(bytes).unwrap();
    }
    zw.finish().unwrap();
    out
}

fn check_imported(store: &Store) {
    let feeds = store.feeds().unwrap();
    assert_eq!(feeds.len(), 2);
    let garden = feeds.iter().find(|f| f.name.as_deref() == Some("Example Garden Blog")).unwrap();
    assert_eq!(garden.page_count, 2);
    assert_eq!(garden.origin, "import");

    let pages = store.list_pages(None, None, 50, 0).unwrap();
    assert_eq!(pages.len(), 3);

    let slow = store.list_pages(Some("compost patience"), None, 10, 0).unwrap();
    assert_eq!(slow.len(), 1, "saved copy text is searchable");
    let slow = &slow[0];
    assert_eq!(slow.title.as_deref(), Some("On slow gardens"));
    assert_eq!(slow.author.as_deref(), Some("A. Gardener"));
    assert_eq!(slow.feed_name.as_deref(), Some("Example Garden Blog"));
    assert_eq!(slow.wayback_status, "done");
    assert_eq!(slow.wayback_url.as_deref(), Some("https://web.archive.org/web/20260301091600/https://example.com/slow-gardens"));
    assert_eq!(slow.ipfs_status, "done");
    assert_eq!(slow.pin_service.as_deref(), Some("pinata"));
    assert!(slow.cid.as_deref().unwrap().starts_with("bafkrei"));
    assert_eq!(slow.origin, "import");
    let body = std::fs::read_to_string(store.file_path(slow).unwrap()).unwrap();
    assert!(body.contains("Compost is patience"));

    let letter = &store.list_pages(Some("lighthouses"), None, 10, 0).unwrap()[0];
    assert_eq!(letter.wayback_status, "failed");
    assert_eq!(letter.wayback_error.as_deref(), Some("Wayback capture timed out"));
    assert_eq!(letter.ipfs_status, "off");

    // Search by feed name works too.
    assert_eq!(store.list_pages(Some("Example Letters"), None, 10, 0).unwrap().len(), 1);

    let never = &store.list_pages(Some("never downloaded"), None, 10, 0).unwrap()[0];
    assert!(never.local_path.is_none());
    assert_eq!(never.wayback_status, "off");
}

#[test]
fn imports_a_folder_and_resyncs_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    let r = import(&store, &fixture()).unwrap();
    assert_eq!((r.feeds_created, r.pages_created, r.copies_saved), (2, 3, 2));
    assert!(r.problems.is_empty(), "{:?}", r.problems);
    check_imported(&store);

    // Second import: nothing new, nothing rewritten.
    let r2 = import(&store, &fixture()).unwrap();
    assert_eq!((r2.feeds_created, r2.feeds_updated), (0, 2));
    assert_eq!((r2.pages_created, r2.pages_updated), (0, 3));
    assert_eq!((r2.copies_saved, r2.copies_unchanged), (0, 2));
    check_imported(&store);
}

#[test]
fn imports_a_zip_even_inside_a_top_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let zip = zip_fixture(tmp.path(), "persistwise-export-2026-11-23/", &[]);
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let r = import(&store, &zip).unwrap();
    assert_eq!(r.pages_created, 3);
    check_imported(&store);
}

#[test]
fn resync_picks_up_changes_from_a_newer_export() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    import(&store, &fixture()).unwrap();

    // A newer export: post renamed, Wayback now done, copy re-fetched.
    let newer = tempfile::tempdir().unwrap();
    copy_dir(&fixture(), newer.path());
    let new_copy = b"<html><head><title>Weekly letter #12</title></head><body><p>Updated: lighthouses and herring.</p></body></html>";
    std::fs::write(newer.path().join("saved-copies/102.html"), new_copy).unwrap();
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(newer.path().join("manifest.json")).unwrap()).unwrap();
    let post = &mut m["posts"][1];
    post["title"] = "Weekly letter #12 (revised)".into();
    post["archive_record"]["wayback_url"] = "https://web.archive.org/web/20260405000000/https://letters.example.net/p/12".into();
    post["saved_copy"]["sha256"] = persistwise_core::util::sha256_hex(new_copy).into();
    std::fs::write(newer.path().join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();

    let r = import(&store, newer.path()).unwrap();
    assert_eq!((r.pages_created, r.copies_saved, r.copies_unchanged), (0, 1, 1));
    let p = &store.list_pages(Some("herring"), None, 10, 0).unwrap()[0];
    assert_eq!(p.title.as_deref(), Some("Weekly letter #12 (revised)"));
    assert_eq!(p.wayback_status, "done");
    assert_eq!(store.list_pages(None, None, 50, 0).unwrap().len(), 3);
}

#[test]
fn checksum_mismatch_and_missing_copy_are_reported() {
    let bad = tempfile::tempdir().unwrap();
    copy_dir(&fixture(), bad.path());
    std::fs::write(bad.path().join("saved-copies/101.html"), "tampered").unwrap();
    std::fs::remove_file(bad.path().join("saved-copies/102.html")).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let r = import(&store, bad.path()).unwrap();
    assert_eq!(r.problems.len(), 1, "{:?}", r.problems);
    assert!(r.problems[0].contains("checksum"));
    assert_eq!(r.copies_missing, 1);
    assert_eq!(r.pages_created, 2, "the bad post is skipped; the others still import");
}

#[test]
fn hostile_zip_entries_are_never_written() {
    let tmp = tempfile::tempdir().unwrap();
    let zip = zip_fixture(tmp.path(), "", &[("../../escape.txt", b"nope"), ("/abs.txt", b"nope")]);
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("archive")).unwrap();
    import(&store, &zip).unwrap();
    assert!(!dir.path().join("escape.txt").exists());
    assert!(!tmp.path().parent().unwrap().join("escape.txt").exists());

    // A manifest that points outside the export is refused per post.
    let evil = tempfile::tempdir().unwrap();
    copy_dir(&fixture(), evil.path());
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(evil.path().join("manifest.json")).unwrap()).unwrap();
    m["posts"][0]["saved_copy"]["path"] = "../../../etc/passwd".into();
    std::fs::write(evil.path().join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    let r = import(&store, evil.path()).unwrap();
    assert!(r.problems.iter().any(|p| p.contains("unsafe path")), "{:?}", r.problems);
}

#[test]
fn rejects_things_that_are_not_exports() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("a")).unwrap();
    let not = tempfile::tempdir().unwrap();
    assert!(import(&store, not.path()).unwrap_err().to_string().contains("manifest.json"));
    std::fs::write(not.path().join("manifest.json"), r#"{"format":"something-else","version":1}"#).unwrap();
    assert!(import(&store, not.path()).unwrap_err().to_string().contains("not a Persistwise export"));
    std::fs::write(not.path().join("manifest.json"), r#"{"format":"persistwise-export","version":99}"#).unwrap();
    assert!(import(&store, not.path()).unwrap_err().to_string().contains("version 99"));
    let junk = not.path().join("junk.zip");
    std::fs::write(&junk, "not a zip").unwrap();
    assert!(import(&store, &junk).is_err());
    assert_eq!(read_manifest(&fixture()).unwrap().posts.len(), 3);
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let e = entry.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}
