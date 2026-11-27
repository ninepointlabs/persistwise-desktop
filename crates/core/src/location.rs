//! Where the archive lives, and moving it somewhere else.
//!
//! The archive folder is found in this order:
//!
//! 1. `PERSISTWISE_DATA_DIR`, when set (tests, portable use, the Linux
//!    background timer).
//! 2. The folder the user chose in Settings. Because `settings.json` lives
//!    inside the archive, that choice is kept in a small pointer file
//!    *outside* it, in the app's config folder ([`pointer_file`]):
//!    Linux `~/.config/persistwise/archive-location.json` (respects
//!    `XDG_CONFIG_HOME`; Flatpak `~/.var/app/com.ninepointlabs.Persistwise/config/persistwise/`),
//!    Windows `%APPDATA%\Nine Point Labs\Persistwise\config\archive-location.json`.
//! 3. The default folder ([`default_dir`]), unchanged from earlier versions.
//!
//! When an archive is moved, a small `persistwise-moved.json` marker is left
//! in the old folder. If a later launch lands on a folder that has that marker
//! and no archive (the pointer file was lost, or an old timer still points
//! there), it follows the marker instead of silently starting a second, empty
//! archive. A chosen folder that is missing (an unplugged drive) is reported
//! as [`Unavailable`], never a reason to create a new empty archive; the app
//! then shows a recovery screen (retry, choose another folder, or the default
//! folder).
//!
//! A folder the user picks that already has other files in it is not used
//! directly: a `Persistwise` subfolder inside it is offered instead
//! ([`FolderKind::NotEmpty`]).

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::store::Store;
use crate::util;

pub const ENV_VAR: &str = "PERSISTWISE_DATA_DIR";
pub const DB_FILE: &str = "persistwise.sqlite3";
pub const MOVED_MARKER: &str = "persistwise-moved.json";
pub const POINTER_FILE: &str = "archive-location.json";
/// Name of the subfolder offered inside a folder that has other files.
pub const SUBFOLDER: &str = "Persistwise";
/// Files that hold the user's settings and keys (not the library).
pub const SETTINGS_FILES: [&str; 2] = ["settings.json", "secrets.json"];
const WRITE_TEST: &str = ".persistwise-write-test";

fn project_dirs() -> Result<directories::ProjectDirs> {
    directories::ProjectDirs::from("com", "Nine Point Labs", "Persistwise")
        .ok_or_else(|| anyhow!("could not work out a home directory"))
}

/// The default archive folder: Linux `~/.local/share/persistwise` (respects
/// `XDG_DATA_HOME`; the Flatpak's is under `~/.var/app/…`), Windows
/// `%APPDATA%\Nine Point Labs\Persistwise\data`.
pub fn default_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().to_path_buf())
}

/// Where the user's choice of archive folder is remembered.
pub fn pointer_file() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join(POINTER_FILE))
}

/// How the archive folder was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Env,
    Chosen,
    Default,
}

#[derive(Debug, Clone, Serialize)]
pub struct Resolved {
    pub path: PathBuf,
    pub source: Source,
    /// Set when a moved-to marker was followed from this folder.
    pub followed_from: Option<PathBuf>,
}

/// The archive folder the app should open is missing (an unplugged drive, a
/// deleted folder). Returned (inside the `anyhow::Error`) by
/// [`Locator::resolve`]; find it with `err.downcast_ref::<Unavailable>()`.
#[derive(Debug, Clone, Serialize)]
pub struct Unavailable {
    /// The folder that should hold the archive.
    pub path: PathBuf,
    pub source: Source,
    /// Set when a moved-to marker led to `path` from this folder.
    pub followed_from: Option<PathBuf>,
    /// The pointer file that names the folder (delete it to go back to the
    /// default folder).
    pub pointer: PathBuf,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "your Persistwise archive folder {} is not available (is its drive connected?). \
             Reconnect it and start Persistwise again, or delete {} to go back to the default folder",
            self.path.display(),
            self.pointer.display()
        )
    }
}

impl std::error::Error for Unavailable {}

#[derive(Debug, Clone)]
pub struct Locator {
    pub env_dir: Option<PathBuf>,
    pub pointer: PathBuf,
    pub default_dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Pointer {
    archive: PathBuf,
    #[serde(default)]
    set_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Marker {
    moved_to: PathBuf,
    #[serde(default)]
    moved_at: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

impl Locator {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            env_dir: std::env::var_os(ENV_VAR)
                .filter(|d| !d.is_empty())
                .map(PathBuf::from),
            pointer: pointer_file()?,
            default_dir: default_dir()?,
        })
    }

    /// The folder stored in the pointer file, if any.
    pub fn chosen(&self) -> Result<Option<PathBuf>> {
        let bytes = match std::fs::read(&self.pointer) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.pointer.display())),
        };
        let p: Pointer = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "{} is damaged; delete it to use the default archive folder",
                self.pointer.display()
            )
        })?;
        if !p.archive.is_absolute() {
            bail!(
                "{} does not hold a full path; delete it to use the default archive folder",
                self.pointer.display()
            );
        }
        Ok(Some(p.archive))
    }

    /// Remembers `dir` as the archive folder. Choosing the default folder
    /// removes the pointer instead.
    pub fn set_chosen(&self, dir: &Path) -> Result<()> {
        if same_dir(dir, &self.default_dir) {
            return self.clear_chosen();
        }
        if let Some(parent) = self.pointer.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let body = serde_json::to_vec_pretty(&Pointer {
            archive: dir.to_path_buf(),
            set_at: Some(util::now()),
        })?;
        let tmp = self.pointer.with_extension("json.part");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.pointer)
            .with_context(|| format!("writing {}", self.pointer.display()))?;
        Ok(())
    }

    pub fn clear_chosen(&self) -> Result<()> {
        match std::fs::remove_file(&self.pointer) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", self.pointer.display()))
            }
            _ => Ok(()),
        }
    }

    /// Works out which folder to open. See the module docs for the order.
    pub fn resolve(&self) -> Result<Resolved> {
        let (start, source) = if let Some(d) = &self.env_dir {
            (d.clone(), Source::Env)
        } else if let Some(d) = self.chosen()? {
            (d, Source::Chosen)
        } else {
            (self.default_dir.clone(), Source::Default)
        };
        let path = follow_markers(&start)?;
        let followed = path != start;
        if (source == Source::Chosen || followed) && !path.join(DB_FILE).is_file() {
            return Err(Unavailable {
                path,
                source,
                followed_from: followed.then_some(start),
                pointer: self.pointer.clone(),
            }
            .into());
        }
        Ok(Resolved {
            path,
            source,
            followed_from: followed.then_some(start),
        })
    }
}

/// Where a moved-to marker in `dir` points, if there is one.
pub fn moved_to(dir: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(dir.join(MOVED_MARKER)).ok()?;
    let m: Marker = serde_json::from_slice(&bytes).ok()?;
    m.moved_to.is_absolute().then_some(m.moved_to)
}

/// Follows moved-to markers from folders that hold no archive.
fn follow_markers(dir: &Path) -> Result<PathBuf> {
    let mut cur = dir.to_path_buf();
    for _ in 0..8 {
        if cur.join(DB_FILE).is_file() {
            return Ok(cur);
        }
        match moved_to(&cur) {
            Some(next) => cur = next,
            None => return Ok(cur),
        }
    }
    bail!(
        "the archive's moved-to notes go round in a circle, starting at {}",
        dir.display()
    )
}

/// An absolute, normalised form of `p`, resolving symlinks for the part that
/// exists.
fn lenient_canonical(p: &Path) -> PathBuf {
    let mut existing = p.to_path_buf();
    let mut rest: Vec<OsString> = Vec::new();
    loop {
        if let Ok(c) = existing.canonicalize() {
            let mut out = c;
            for r in rest.iter().rev() {
                out.push(r);
            }
            return out;
        }
        match (
            existing.file_name().map(|f| f.to_os_string()),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => {
                // Nothing exists; normalise lexically.
                let mut out = PathBuf::new();
                for c in p.components() {
                    match c {
                        Component::ParentDir => {
                            out.pop();
                        }
                        Component::CurDir => {}
                        other => out.push(other.as_os_str()),
                    }
                }
                return out;
            }
        }
    }
}

pub fn same_dir(a: &Path, b: &Path) -> bool {
    lenient_canonical(a) == lenient_canonical(b)
}

/// What a folder the user picked holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderKind {
    /// Already a Persistwise archive: it is opened as it is.
    Existing,
    /// Empty (or new): the current archive can be moved or copied there, or
    /// a fresh one started.
    Empty,
    /// Has other files and no archive: not used directly. A `Persistwise`
    /// subfolder inside it is offered instead (see [`check_folder`]).
    #[serde(rename = "notempty")]
    NotEmpty,
}

/// What [`check_folder`] found, for the folder-choice screens.
#[derive(Debug, Clone, Serialize)]
pub struct FolderCheck {
    pub path: PathBuf,
    pub kind: FolderKind,
    /// For [`FolderKind::NotEmpty`]: the `Persistwise` subfolder to use
    /// instead, and what that holds (never `NotEmpty`).
    pub subfolder: Option<PathBuf>,
    pub subfolder_kind: Option<FolderKind>,
}

/// Checks `dest` as a new archive folder (see [`check_destination`]) and,
/// when it has other files in it, checks the `Persistwise` subfolder that
/// would be used instead. Nothing is created.
pub fn check_folder(current: &Path, dest: &Path) -> Result<FolderCheck> {
    let kind = check_destination(current, dest)?;
    let path = lenient_canonical(dest);
    if kind != FolderKind::NotEmpty {
        return Ok(FolderCheck {
            path,
            kind,
            subfolder: None,
            subfolder_kind: None,
        });
    }
    let sub = path.join(SUBFOLDER);
    let sub_kind = check_destination(current, &sub)?;
    if sub_kind == FolderKind::NotEmpty {
        bail!(
            "{} already has other files in it, and so does its {SUBFOLDER} folder. Choose an empty folder or one that holds a Persistwise archive",
            path.display()
        );
    }
    Ok(FolderCheck {
        path,
        kind,
        subfolder: Some(sub),
        subfolder_kind: Some(sub_kind),
    })
}

/// Checks that `dest` is a safe new archive folder when the archive is
/// currently in `current` (or, when that folder is missing, should be), and
/// says what is there. Refuses the current folder, a folder inside it or
/// containing it, a file, a relative path and an unwritable place. Nothing is
/// left behind: a folder created to test writing is removed again.
pub fn check_destination(current: &Path, dest: &Path) -> Result<FolderKind> {
    if !dest.is_absolute() {
        bail!("choose a full folder path");
    }
    if dest.exists() && !dest.is_dir() {
        bail!("{} is a file, not a folder", dest.display());
    }
    let cur = lenient_canonical(current);
    let d = lenient_canonical(dest);
    if d == cur {
        bail!("that is already your archive folder");
    }
    if d.starts_with(&cur) {
        bail!(
            "that folder is inside your current archive ({}); choose a folder outside it",
            cur.display()
        );
    }
    if cur.starts_with(&d) {
        bail!(
            "that folder contains your current archive ({}); choose a separate, empty folder",
            cur.display()
        );
    }
    let kind = if d.join(DB_FILE).is_file() {
        FolderKind::Existing
    } else if d.is_dir()
        && std::fs::read_dir(&d)
            .with_context(|| format!("cannot read {}", d.display()))?
            .filter_map(|e| e.ok())
            .any(|e| e.file_name() != MOVED_MARKER)
    {
        FolderKind::NotEmpty
    } else {
        FolderKind::Empty
    };
    // Can we write there? Folders created only for this check are removed
    // again, so looking at a folder never leaves anything behind.
    let first_missing = d
        .ancestors()
        .take_while(|a| !a.exists())
        .last()
        .map(Path::to_path_buf);
    std::fs::create_dir_all(&d).with_context(|| format!("cannot create {}", d.display()))?;
    let probe = d.join(WRITE_TEST);
    let wrote = std::fs::write(&probe, b"ok").and_then(|_| std::fs::remove_file(&probe));
    if let Some(top) = first_missing {
        let mut cur = d.clone();
        loop {
            let _ = std::fs::remove_dir(&cur);
            if cur == top {
                break;
            }
            match cur.parent() {
                Some(p) => cur = p.to_path_buf(),
                None => break,
            }
        }
    }
    wrote.map_err(|e| anyhow!("Persistwise cannot write to {}: {e}", d.display()))?;
    Ok(kind)
}

/// What to do with the current archive when switching folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transfer {
    /// Open the archive already in the new folder; the current one stays put.
    Open,
    /// Copy the archive to the new folder, then remove it from the old one.
    Move,
    /// Copy the archive; both copies stay usable.
    Copy,
    /// Start an empty archive in the new folder with default settings; the
    /// current one stays put.
    Fresh,
    /// Start an empty library in the new folder but keep the current
    /// settings and keys ([`SETTINGS_FILES`]); the current archive stays put.
    #[serde(rename = "fresh_keep_settings")]
    FreshKeepSettings,
}

/// Result of [`prepare`]: the new folder is ready to be opened.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub dest: PathBuf,
    pub how: Transfer,
    /// Top-level entries copied from the old folder (besides the database).
    pub copied: Vec<OsString>,
}

/// Gets `dest` ready as the archive folder. Nothing in the current archive is
/// changed here; for a move, call [`finish_move`] once the app has switched
/// to the new folder.
pub fn prepare(store: &Store, dest: &Path, how: Transfer) -> Result<Prepared> {
    let kind = check_destination(store.root(), dest)?;
    let dest = lenient_canonical(dest);
    let mut copied = Vec::new();
    if kind == FolderKind::NotEmpty {
        bail!(not_empty_message(&dest));
    }
    std::fs::create_dir_all(&dest).with_context(|| format!("cannot create {}", dest.display()))?;
    match (kind, how) {
        (FolderKind::Existing, Transfer::Open) => {
            // Check it opens (and is not from a newer version) before switching.
            Store::open(&dest)?;
        }
        (FolderKind::Existing, _) => bail!(
            "{} already holds a Persistwise archive; it can only be opened as it is",
            dest.display()
        ),
        (FolderKind::Empty, Transfer::Open) => bail!(
            "{} has no archive yet; choose whether to move, copy or start a fresh one",
            dest.display()
        ),
        (FolderKind::Empty, Transfer::Fresh) => {
            let _ = std::fs::remove_file(dest.join(MOVED_MARKER));
            Store::open(&dest)?;
        }
        (FolderKind::Empty, Transfer::FreshKeepSettings) => {
            let _ = std::fs::remove_file(dest.join(MOVED_MARKER));
            if let Err(e) = copy_settings(store.root(), &dest).and_then(|_| Store::open(&dest)) {
                empty_folder(&dest);
                return Err(e);
            }
        }
        (FolderKind::NotEmpty, _) => unreachable!("refused above"),
        (FolderKind::Empty, Transfer::Move | Transfer::Copy) => match copy_archive(store, &dest) {
            Ok(c) => copied = c,
            Err(e) => {
                empty_folder(&dest);
                return Err(e);
            }
        },
    }
    Ok(Prepared { dest, how, copied })
}

fn not_empty_message(dest: &Path) -> String {
    format!(
        "{} already has other files in it. Use the {SUBFOLDER} folder inside it, choose an empty folder, or choose a folder that already holds a Persistwise archive",
        dest.display()
    )
}

/// Like [`prepare`], when no archive is open because its folder (`missing`)
/// is not available. Only [`Transfer::Open`] (an existing archive) and
/// [`Transfer::Fresh`] (an explicit new, empty archive) are possible:
/// there is nothing to move or copy, and no settings to keep.
pub fn prepare_without_archive(missing: &Path, dest: &Path, how: Transfer) -> Result<Prepared> {
    let kind = check_destination(missing, dest)?;
    let dest = lenient_canonical(dest);
    match (kind, how) {
        (FolderKind::NotEmpty, _) => bail!(not_empty_message(&dest)),
        (FolderKind::Existing, Transfer::Open) => {
            Store::open(&dest)?;
        }
        (FolderKind::Existing, _) => bail!(
            "{} already holds a Persistwise archive; it can only be opened as it is",
            dest.display()
        ),
        (FolderKind::Empty, Transfer::Fresh) => {
            std::fs::create_dir_all(&dest)
                .with_context(|| format!("cannot create {}", dest.display()))?;
            let _ = std::fs::remove_file(dest.join(MOVED_MARKER));
            Store::open(&dest)?;
        }
        (FolderKind::Empty, _) => bail!(
            "{} has no archive yet, and your archive's folder is not available to move or copy from; reconnect it, or start a new, empty archive here",
            dest.display()
        ),
    }
    Ok(Prepared {
        dest,
        how,
        copied: Vec::new(),
    })
}

/// Copies the settings and keys (not the library) into a new archive folder.
fn copy_settings(from: &Path, to: &Path) -> Result<()> {
    for name in SETTINGS_FILES {
        let src = from.join(name);
        if src.is_file() {
            // Keeps permissions, so secrets.json stays owner-only.
            std::fs::copy(&src, to.join(name))
                .with_context(|| format!("copying {}", src.display()))?;
        }
    }
    Ok(())
}

fn skip_on_copy(name: &str) -> bool {
    name == DB_FILE
        || name == format!("{DB_FILE}-wal")
        || name == format!("{DB_FILE}-shm")
        || name == format!("{DB_FILE}-journal")
        || name == MOVED_MARKER
        || name == WRITE_TEST
        || name.ends_with(".part")
}

fn copy_archive(store: &Store, dest: &Path) -> Result<Vec<OsString>> {
    let _ = std::fs::remove_file(dest.join(MOVED_MARKER));
    store.copy_database_to(&dest.join(DB_FILE))?;
    let mut copied = Vec::new();
    for entry in std::fs::read_dir(store.root())? {
        let entry = entry?;
        let name = entry.file_name();
        if skip_on_copy(&name.to_string_lossy()) {
            continue;
        }
        copy_tree(&entry.path(), &dest.join(&name))?;
        copied.push(name);
    }
    // Check the copy before anything relies on it.
    let new = Store::open(dest).context("opening the copied archive")?;
    let (a, b) = (store.stats()?, new.stats()?);
    if a.pages != b.pages || a.feeds != b.feeds {
        bail!(
            "the copy does not match ({} pages and {} feeds, expected {} and {})",
            b.pages,
            b.feeds,
            a.pages,
            a.feeds
        );
    }
    for (rel, size) in store.local_files()? {
        let src = store.root().join(&rel);
        if !src.is_file() {
            continue; // already missing in the old archive
        }
        let got = std::fs::metadata(dest.join(&rel)).map(|m| m.len() as i64);
        match got {
            Ok(n) if size < 0 || n == size => {}
            _ => bail!("the saved copy {rel} did not copy correctly"),
        }
    }
    Ok(copied)
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().ends_with(".part") {
                continue;
            }
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else if meta.is_file() {
        // Keeps permissions, so secrets.json stays owner-only.
        std::fs::copy(src, dst).with_context(|| format!("copying {}", src.display()))?;
    }
    Ok(())
}

/// Removes everything inside `dir` (used to undo a failed copy into a folder
/// that was empty).
fn empty_folder(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let _ = if p.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
        }
    }
}

/// Completes a move once nothing uses the old archive any more: removes the
/// old database and the copied files, and leaves a moved-to marker so a later
/// launch that lands on the old folder finds the new one.
pub fn finish_move(old_root: &Path, prepared: &Prepared) -> Result<()> {
    if prepared.how != Transfer::Move {
        return Ok(());
    }
    let mut problems = Vec::new();
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let f = old_root.join(format!("{DB_FILE}{suffix}"));
        if let Err(e) = std::fs::remove_file(&f) {
            if e.kind() != std::io::ErrorKind::NotFound {
                problems.push(format!("{}: {e}", f.display()));
            }
        }
    }
    let marker = Marker {
        moved_to: prepared.dest.clone(),
        moved_at: Some(util::now()),
        note: Some(
            "This Persistwise archive was moved. Persistwise opens the folder above instead."
                .into(),
        ),
    };
    if let Err(e) = std::fs::write(
        old_root.join(MOVED_MARKER),
        serde_json::to_vec_pretty(&marker)?,
    ) {
        problems.push(format!("moved-to note: {e}"));
    }
    for name in &prepared.copied {
        let p = old_root.join(name);
        let r = if p.is_dir() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        if let Err(e) = r {
            if e.kind() != std::io::ErrorKind::NotFound {
                problems.push(format!("{}: {e}", p.display()));
            }
        }
    }
    if !problems.is_empty() {
        bail!(
            "the archive was moved, but some of the old files could not be removed (you can delete them yourself): {}",
            problems.join("; ")
        );
    }
    Ok(())
}
