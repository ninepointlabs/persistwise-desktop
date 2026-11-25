//! Choosing, resolving and moving the archive folder.

use persistwise_core::location::{
    self, check_destination, finish_move, prepare, FolderKind, Locator, Source, Transfer, DB_FILE,
    MOVED_MARKER,
};
use persistwise_core::settings::{self, Secrets};
use persistwise_core::store::{NewPage, Store};
use persistwise_core::util;
use std::path::{Path, PathBuf};

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        Self { _tmp: tmp, root }
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
    fn locator(&self, env_dir: Option<PathBuf>) -> Locator {
        Locator {
            env_dir,
            pointer: self.p("config/persistwise/archive-location.json"),
            default_dir: self.p("data/persistwise"),
        }
    }
}

fn add_page(store: &Store, url: &str, body: &str) -> i64 {
    let uid = util::new_uid();
    let file = store.write_file(&uid, "html", body.as_bytes()).unwrap();
    store
        .insert_page(
            NewPage {
                url: url.into(),
                title: Some("A page".into()),
                content_type: "text/html".into(),
                body_text: body.into(),
                origin: "desktop".into(),
                ..Default::default()
            },
            Some(file),
            &uid,
        )
        .unwrap()
        .id
}

/// A default-location archive with a page, settings and a secret.
fn seeded(dir: &Path) -> Store {
    let store = Store::open(dir).unwrap();
    add_page(&store, "https://a.test/", "kingfishers dive");
    let mut s = settings::load(dir);
    s.wayback_on_save = false;
    settings::save(dir, &s).unwrap();
    settings::save_secrets(
        dir,
        &Secrets {
            pinata_jwt: Some("jwt-secret".into()),
            ..Default::default()
        },
    )
    .unwrap();
    store
}

#[test]
fn default_is_unchanged_without_pointer_or_env() {
    let e = Env::new();
    let r = e.locator(None).resolve().unwrap();
    assert_eq!(r.path, e.p("data/persistwise"));
    assert_eq!(r.source, Source::Default);
    // The default folder itself is computed exactly as before.
    let expected = directories::ProjectDirs::from("com", "Nine Point Labs", "Persistwise")
        .unwrap()
        .data_dir()
        .to_path_buf();
    assert_eq!(location::default_dir().unwrap(), expected);
    #[cfg(target_os = "linux")]
    assert!(expected.ends_with("persistwise"));
    #[cfg(windows)]
    assert!(expected.ends_with("Nine Point Labs\\Persistwise\\data"));
}

#[test]
fn stored_pointer_is_used_and_env_still_wins() {
    let e = Env::new();
    let chosen = e.p("usb/archive");
    Store::open(&chosen).unwrap();
    let loc = e.locator(None);
    loc.set_chosen(&chosen).unwrap();
    let r = loc.resolve().unwrap();
    assert_eq!((r.path.clone(), r.source), (chosen.clone(), Source::Chosen));

    let env = e.p("portable");
    let r = e.locator(Some(env.clone())).resolve().unwrap();
    assert_eq!((r.path, r.source), (env, Source::Env));

    // Choosing the default folder again removes the pointer.
    loc.set_chosen(&e.p("data/persistwise")).unwrap();
    assert!(!loc.pointer.exists());
    assert_eq!(loc.resolve().unwrap().source, Source::Default);
}

#[test]
fn a_missing_chosen_folder_is_an_error_not_a_new_archive() {
    let e = Env::new();
    let loc = e.locator(None);
    let gone = e.p("unplugged/archive");
    loc.set_chosen(&gone).unwrap();
    let err = loc.resolve().unwrap_err().to_string();
    assert!(err.contains("not available"), "{err}");
    assert!(!gone.exists());
}

#[test]
fn move_puts_everything_in_the_new_folder_and_leaves_a_marker() {
    let e = Env::new();
    let old = e.p("data/persistwise");
    let new = e.p("Documents/Archive");
    std::fs::create_dir_all(&new).unwrap();
    let store = seeded(&old);
    assert_eq!(check_destination(&old, &new).unwrap(), FolderKind::Empty);

    let prepared = prepare(&store, &new, Transfer::Move).unwrap();
    drop(store);
    finish_move(&old, &prepared).unwrap();
    let loc = e.locator(None);
    loc.set_chosen(&prepared.dest).unwrap();

    // New folder: database, saved copies, settings and secrets.
    let r = loc.resolve().unwrap();
    assert_eq!(r.path, new);
    let store = Store::open(&r.path).unwrap();
    let pages = store.list_pages(Some("kingfishers"), None, 10, 0).unwrap();
    assert_eq!(pages.len(), 1);
    assert!(store.file_path(&pages[0]).is_some());
    assert!(!settings::load(&new).wayback_on_save);
    assert_eq!(
        settings::load_secrets(&new).pinata_jwt.as_deref(),
        Some("jwt-secret")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(new.join("secrets.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // Old folder: only the moved-to marker, no second live archive.
    assert!(!old.join(DB_FILE).exists());
    assert!(!old.join("pages").exists());
    assert!(!old.join("secrets.json").exists());
    assert_eq!(location::moved_to(&old), Some(new.clone()));
    let left: Vec<_> = std::fs::read_dir(&old)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(left, vec![std::ffi::OsString::from(MOVED_MARKER)]);

    // Even without the pointer, a launch on the old folder finds the new one.
    loc.clear_chosen().unwrap();
    let r = loc.resolve().unwrap();
    assert_eq!(r.path, new);
    assert_eq!(r.followed_from, Some(old.clone()));
    // So does an old timer that still points at the old folder.
    assert_eq!(e.locator(Some(old.clone())).resolve().unwrap().path, new);
}

#[test]
fn copy_leaves_two_usable_archives() {
    let e = Env::new();
    let old = e.p("data/persistwise");
    let new = e.p("Backup/Archive");
    let store = seeded(&old);
    let prepared = prepare(&store, &new, Transfer::Copy).unwrap();
    finish_move(&old, &prepared).unwrap(); // a no-op for a copy
    let loc = e.locator(None);
    loc.set_chosen(&prepared.dest).unwrap();
    assert_eq!(loc.resolve().unwrap().path, new);

    for dir in [&old, &new] {
        let s = Store::open(dir).unwrap();
        let pages = s.list_pages(Some("kingfishers"), None, 10, 0).unwrap();
        assert_eq!(pages.len(), 1, "{}", dir.display());
        assert!(s.file_path(&pages[0]).is_some());
        assert_eq!(
            settings::load_secrets(dir).pinata_jwt.as_deref(),
            Some("jwt-secret")
        );
    }
    // The copies are independent.
    add_page(
        &Store::open(&new).unwrap(),
        "https://b.test/",
        "only in the copy",
    );
    assert_eq!(store.stats().unwrap().pages, 1);
}

#[test]
fn start_fresh_leaves_the_old_archive_alone() {
    let e = Env::new();
    let old = e.p("data/persistwise");
    let new = e.p("Fresh");
    let store = seeded(&old);
    let prepared = prepare(&store, &new, Transfer::Fresh).unwrap();
    finish_move(&old, &prepared).unwrap();

    let fresh = Store::open(&new).unwrap();
    assert_eq!(fresh.stats().unwrap().pages, 0);
    assert!(settings::load_secrets(&new).pinata_jwt.is_none());
    assert_eq!(store.stats().unwrap().pages, 1);
    assert!(old.join(DB_FILE).exists());
    assert_eq!(
        settings::load_secrets(&old).pinata_jwt.as_deref(),
        Some("jwt-secret")
    );
    assert!(!old.join(MOVED_MARKER).exists());
}

#[test]
fn an_existing_archive_is_opened_not_overwritten() {
    let e = Env::new();
    let old = e.p("data/persistwise");
    let other = e.p("Other");
    let store = seeded(&old);
    {
        let o = Store::open(&other).unwrap();
        add_page(&o, "https://other.test/", "herons only here");
    }
    assert_eq!(
        check_destination(&old, &other).unwrap(),
        FolderKind::Existing
    );
    assert!(prepare(&store, &other, Transfer::Move).is_err());
    assert!(prepare(&store, &other, Transfer::Copy).is_err());
    prepare(&store, &other, Transfer::Open).unwrap();

    let o = Store::open(&other).unwrap();
    assert_eq!(o.stats().unwrap().pages, 1);
    assert_eq!(o.list_pages(Some("herons"), None, 10, 0).unwrap().len(), 1);
    assert!(o
        .list_pages(Some("kingfishers"), None, 10, 0)
        .unwrap()
        .is_empty());
    assert_eq!(store.stats().unwrap().pages, 1);
}

#[test]
fn unsafe_destinations_are_refused() {
    let e = Env::new();
    let old = e.p("data/persistwise");
    let store = seeded(&old);
    let refuse = |dest: &Path, why: &str| {
        let err = prepare(&store, dest, Transfer::Move)
            .unwrap_err()
            .to_string();
        assert!(err.contains(why), "{}: {err}", dest.display());
    };
    refuse(&old, "already your archive folder");
    refuse(&old.join("pages"), "inside your current archive");
    refuse(&old.join("pages/new"), "inside your current archive");
    refuse(&e.p("data"), "contains your current archive");
    refuse(Path::new("relative/folder"), "full folder path");
    let busy = e.p("Busy");
    std::fs::create_dir_all(&busy).unwrap();
    std::fs::write(busy.join("notes.txt"), "mine").unwrap();
    refuse(&busy, "already has other files");
    assert_eq!(
        std::fs::read_to_string(busy.join("notes.txt")).unwrap(),
        "mine"
    );
    let file = e.p("a-file");
    std::fs::write(&file, "x").unwrap();
    refuse(&file, "not a folder");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let ro = e.p("ReadOnly");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root can write anywhere; only check as a normal user.
        if std::fs::write(ro.join("probe"), "x").is_err() {
            refuse(&ro, "cannot write");
        }
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Nothing was touched.
    assert_eq!(store.stats().unwrap().pages, 1);
    assert!(old.join(DB_FILE).exists());
}

#[test]
fn moving_back_to_the_default_folder_reuses_it() {
    let e = Env::new();
    let default = e.p("data/persistwise");
    let usb = e.p("usb/archive");
    let loc = e.locator(None);
    // Move default -> usb.
    let store = seeded(&default);
    let p = prepare(&store, &usb, Transfer::Move).unwrap();
    drop(store);
    finish_move(&default, &p).unwrap();
    loc.set_chosen(&p.dest).unwrap();
    // And back: the default folder only holds the marker, so it counts as empty.
    let store = Store::open(loc.resolve().unwrap().path).unwrap();
    assert_eq!(
        check_destination(&usb, &default).unwrap(),
        FolderKind::Empty
    );
    let p = prepare(&store, &default, Transfer::Move).unwrap();
    drop(store);
    finish_move(&usb, &p).unwrap();
    loc.set_chosen(&p.dest).unwrap();
    assert!(!loc.pointer.exists());
    let r = loc.resolve().unwrap();
    assert_eq!(
        (r.path.clone(), r.source),
        (default.clone(), Source::Default)
    );
    assert!(!default.join(MOVED_MARKER).exists());
    assert_eq!(Store::open(&r.path).unwrap().stats().unwrap().pages, 1);
}
