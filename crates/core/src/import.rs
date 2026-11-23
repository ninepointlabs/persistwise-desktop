//! Imports a Persistwise web account export (format `persistwise-export`,
//! version 1). See `docs/import-format.md` for the format.
//!
//! The export is a `.zip` (or the same files unpacked into a folder):
//!
//! ```text
//! manifest.json
//! saved-copies/<archive_record_id>.html
//! ```
//!
//! Importing is idempotent: running it again with a newer export updates
//! what changed and adds what is new (keyed by the web app's ids), so a
//! local copy can be kept in sync until the web account is retired.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Component, Path};

use crate::store::{NewPage, Store};
use crate::{text, util};

pub const FORMAT: &str = "persistwise-export";
pub const VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;
const MAX_COPY_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub exported_at: Option<String>,
    #[serde(default)]
    pub source: Source,
    #[serde(default)]
    pub feeds: Vec<ExFeed>,
    #[serde(default)]
    pub posts: Vec<ExPost>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct Source {
    pub app: Option<String>,
    pub host: Option<String>,
    pub app_version: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ExFeed {
    pub id: i64,
    pub url: String,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub last_polled_at: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ExPost {
    pub id: i64,
    pub feed_id: Option<i64>,
    pub guid: Option<String>,
    pub url: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<String>,
    pub fetched_at: Option<String>,
    pub created_at: Option<String>,
    pub status: Option<String>,
    pub archive_record: Option<ExArchiveRecord>,
    pub saved_copy: Option<ExSavedCopy>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ExArchiveRecord {
    pub id: i64,
    pub original_url: Option<String>,
    pub status: Option<String>,
    pub wayback_url: Option<String>,
    pub wayback_submitted_at: Option<String>,
    pub wayback_failed_at: Option<String>,
    pub cid: Option<String>,
    pub pin_service: Option<String>,
    pub archived_at: Option<String>,
    pub pin_failed_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ExSavedCopy {
    /// Path inside the export, e.g. `saved-copies/42.html`.
    pub path: String,
    pub sha256: Option<String>,
    pub bytes: Option<u64>,
    pub content_type: Option<String>,
}

#[derive(Debug, Default, Serialize, Clone, PartialEq)]
pub struct Report {
    pub feeds_created: usize,
    pub feeds_updated: usize,
    pub pages_created: usize,
    pub pages_updated: usize,
    pub copies_saved: usize,
    pub copies_unchanged: usize,
    pub copies_missing: usize,
    pub problems: Vec<String>,
}

/// Where the export's files come from.
enum Source_ {
    Dir(std::path::PathBuf),
    Zip(zip::ZipArchive<std::fs::File>),
}

impl Source_ {
    fn open(path: &Path) -> Result<Self> {
        if path.is_dir() {
            return Ok(Self::Dir(path.to_path_buf()));
        }
        let f = std::fs::File::open(path)
            .with_context(|| format!("could not open {}", path.display()))?;
        let z = zip::ZipArchive::new(f).map_err(|_| anyhow!("that file is not a zip export"))?;
        Ok(Self::Zip(z))
    }

    fn read(&mut self, name: &str, max: u64) -> Result<Option<Vec<u8>>> {
        let rel = safe_rel(name).ok_or_else(|| anyhow!("unsafe path in export: {name}"))?;
        match self {
            Self::Dir(root) => {
                let p = root.join(&rel);
                if !p.is_file() {
                    return Ok(None);
                }
                if std::fs::metadata(&p)?.len() > max {
                    bail!("{name} is larger than allowed");
                }
                Ok(Some(std::fs::read(p)?))
            }
            Self::Zip(z) => {
                // Also accept exports whose files sit inside one top folder.
                let names: Vec<String> = z
                    .file_names()
                    .filter_map(|n| n.ok().map(|n| n.into_owned()))
                    .collect();
                let found = names
                    .iter()
                    .find(|n| n.as_str() == rel)
                    .or_else(|| {
                        names
                            .iter()
                            .find(|n| n.split_once('/').map(|(_, r)| r) == Some(rel.as_str()))
                    })
                    .cloned();
                let Some(found) = found else { return Ok(None) };
                let entry = z.by_name(&found)?;
                if entry.size() > max {
                    bail!("{name} is larger than allowed");
                }
                let mut buf = Vec::with_capacity(entry.size() as usize);
                entry.take(max + 1).read_to_end(&mut buf)?;
                if buf.len() as u64 > max {
                    bail!("{name} is larger than allowed");
                }
                Ok(Some(buf))
            }
        }
    }
}

/// Only plain relative paths, no `..`, no absolute paths, no backslashes.
fn safe_rel(name: &str) -> Option<String> {
    if name.is_empty() || name.contains('\\') || name.contains('\0') {
        return None;
    }
    let p = Path::new(name);
    if p.components().all(|c| matches!(c, Component::Normal(_))) {
        Some(name.to_string())
    } else {
        None
    }
}

pub fn read_manifest(path: &Path) -> Result<Manifest> {
    let mut src = Source_::open(path)?;
    parse_manifest(&mut src)
}

fn parse_manifest(src: &mut Source_) -> Result<Manifest> {
    let bytes = src
        .read("manifest.json", MAX_MANIFEST_BYTES)?
        .ok_or_else(|| anyhow!("no manifest.json in the export"))?;
    let m: Manifest = serde_json::from_slice(&bytes).context("manifest.json is not valid")?;
    if m.format != FORMAT {
        bail!(
            "this is not a Persistwise export (format is {:?})",
            m.format
        );
    }
    if m.version != VERSION {
        bail!(
            "this export is version {}; this app reads version {VERSION}",
            m.version
        );
    }
    Ok(m)
}

/// Imports (or re-syncs) an export into the local archive.
pub fn import(store: &Store, path: &Path) -> Result<Report> {
    let mut src = Source_::open(path)?;
    let m = parse_manifest(&mut src)?;
    let host = m
        .source
        .host
        .clone()
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| "persistwise.com".into());
    let mut report = Report::default();
    let mut feed_ids = std::collections::HashMap::new();

    for f in &m.feeds {
        let key = format!("{host}:feed:{}", f.id);
        let (id, created) = store.upsert_feed(
            &key,
            &f.url,
            f.name.as_deref(),
            f.kind.as_deref(),
            f.status.as_deref(),
            f.last_polled_at.as_deref(),
            f.created_at.as_deref(),
        )?;
        feed_ids.insert(f.id, id);
        if created {
            report.feeds_created += 1;
        } else {
            report.feeds_updated += 1;
        }
    }

    for p in &m.posts {
        if let Err(e) = import_post(store, &mut src, &host, p, &feed_ids, &mut report) {
            report
                .problems
                .push(format!("post {} ({}): {e:#}", p.id, p.url));
        }
    }
    Ok(report)
}

fn import_post(
    store: &Store,
    src: &mut Source_,
    host: &str,
    p: &ExPost,
    feed_ids: &std::collections::HashMap<i64, i64>,
    report: &mut Report,
) -> Result<()> {
    let key = format!("{host}:post:{}", p.id);
    let mut existing = store.page_by_remote_key(&key)?;
    // The same entry may already have been saved by local feed polling.
    if existing.is_none() {
        if let (Some(fid), Some(guid)) = (
            p.feed_id.and_then(|f| feed_ids.get(&f).copied()),
            p.guid.as_deref(),
        ) {
            if let Some(page) = store.page_by_feed_guid(fid, guid)? {
                store.set_remote_key(page.id, &key)?;
                existing = Some(page);
            }
        }
    }

    // The saved copy, verified against its checksum when one is given.
    let mut copy: Option<(Vec<u8>, String)> = None;
    if let Some(sc) = &p.saved_copy {
        match src.read(&sc.path, MAX_COPY_BYTES)? {
            Some(bytes) => {
                let sha = util::sha256_hex(&bytes);
                if let Some(want) = sc.sha256.as_deref() {
                    if !want.eq_ignore_ascii_case(&sha) {
                        bail!("saved copy {} does not match its checksum", sc.path);
                    }
                }
                copy = Some((bytes, sha));
            }
            None => report.copies_missing += 1,
        }
    }

    let content_type = p
        .saved_copy
        .as_ref()
        .and_then(|s| s.content_type.clone())
        .unwrap_or_else(|| "text/html".into());
    let ext = crate::capture::ext_for(&content_type);
    let body_text = match &copy {
        Some((bytes, _)) if ext == "html" => text::extract(&util::decode_html(bytes, None)).text,
        _ => String::new(),
    };
    let new = NewPage {
        url: p.url.clone(),
        final_url: None,
        title: p.title.clone(),
        author: p.author.clone(),
        feed_id: p.feed_id.and_then(|f| feed_ids.get(&f).copied()),
        guid: p.guid.clone(),
        published_at: p.published_at.clone(),
        saved_at: p.fetched_at.clone().or_else(|| p.created_at.clone()),
        content_type,
        body_text,
        assets_inlined: 0,
        assets_failed: 0,
        origin: "import".into(),
        remote_key: Some(key),
    };

    let page = match existing {
        Some(page) => {
            // A copy this computer saved itself (fully inlined) is kept over
            // the web app's HTML; imported copies are refreshed when changed.
            let local_is_better = page.origin != "import" && store.file_path(&page).is_some();
            let file = match &copy {
                Some(_) if local_is_better => {
                    report.copies_unchanged += 1;
                    None
                }
                Some((bytes, sha))
                    if page.sha256.as_deref() != Some(sha.as_str())
                        || store.file_path(&page).is_none() =>
                {
                    report.copies_saved += 1;
                    Some(store.write_file(&page.uid, ext, bytes)?)
                }
                Some(_) => {
                    report.copies_unchanged += 1;
                    None
                }
                None => None,
            };
            store.update_imported_page(page.id, &new, file)?;
            report.pages_updated += 1;
            page
        }
        None => {
            let uid = util::new_uid();
            let file = match &copy {
                Some((bytes, _)) => {
                    report.copies_saved += 1;
                    Some(store.write_file(&uid, ext, bytes)?)
                }
                None => None,
            };
            report.pages_created += 1;
            store.insert_page(new, file, &uid)?
        }
    };

    // Wayback and IPFS results exactly as the web app recorded them.
    let ar = p.archive_record.as_ref();
    let wb_url = ar
        .and_then(|a| a.wayback_url.as_deref())
        .filter(|s| !s.is_empty());
    let cid = ar.and_then(|a| a.cid.as_deref()).filter(|s| !s.is_empty());
    let err = ar.and_then(|a| a.last_error.as_deref());
    let wb_status = match (wb_url, ar.and_then(|a| a.wayback_failed_at.as_deref())) {
        (Some(_), _) => "done",
        (None, Some(_)) => "failed",
        _ => "off",
    };
    let ipfs_status = match (cid, ar.and_then(|a| a.pin_failed_at.as_deref())) {
        (Some(_), _) => "done",
        (None, Some(_)) => "failed",
        _ => "off",
    };
    store.set_backup_fields(
        page.id,
        (
            wb_status,
            wb_url,
            if wb_status == "failed" { err } else { None },
            ar.and_then(|a| a.wayback_submitted_at.as_deref()),
        ),
        (
            ipfs_status,
            cid,
            ar.and_then(|a| a.pin_service.as_deref()),
            if ipfs_status == "failed" { err } else { None },
            ar.and_then(|a| a.archived_at.as_deref()),
        ),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_paths() {
        assert!(safe_rel("saved-copies/1.html").is_some());
        assert!(safe_rel("../etc/passwd").is_none());
        assert!(safe_rel("/etc/passwd").is_none());
        assert!(safe_rel("a\\b").is_none());
        assert!(safe_rel("saved-copies/../../x").is_none());
    }
}
