//! The local archive: one SQLite database plus a folder of saved files.
//!
//! Layout under the data directory:
//!
//! ```text
//! persistwise.sqlite3      metadata + full-text index
//! pages/<ab>/<uid>.html    saved copies (one self-contained file per page)
//! settings.json            non-secret settings
//! secrets.json             pinning keys the user pasted in (mode 0600 on Unix)
//! ```
//!
//! Pages saved together as a multi-page thread are grouped: `threads` holds
//! one row per thread and `thread_pages` puts each page (at most one thread
//! per page) at a position in it.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::util;

const SCHEMA_VERSION: i64 = 4;

/// Marks where a search hit starts and ends in a snippet. Private-use
/// characters, so the UI can HTML-escape the snippet and then swap these
/// for `<mark>` safely.
pub const HIT_START: char = '\u{E000}';
pub const HIT_END: char = '\u{E001}';

pub struct Store {
    root: PathBuf,
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Feed {
    pub id: i64,
    pub url: String,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub last_polled_at: Option<String>,
    pub created_at: String,
    pub origin: String,
    pub page_count: i64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Page {
    pub id: i64,
    pub uid: String,
    pub url: String,
    pub final_url: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub feed_id: Option<i64>,
    pub feed_name: Option<String>,
    pub guid: Option<String>,
    pub published_at: Option<String>,
    pub saved_at: String,
    pub content_type: String,
    pub local_path: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub assets_inlined: i64,
    pub assets_failed: i64,
    /// off | pending | done | failed
    pub wayback_status: String,
    pub wayback_url: Option<String>,
    pub wayback_error: Option<String>,
    pub wayback_at: Option<String>,
    /// off | pending | done | failed
    pub ipfs_status: String,
    pub cid: Option<String>,
    pub pin_service: Option<String>,
    pub ipfs_error: Option<String>,
    pub ipfs_at: Option<String>,
    /// desktop | import | feed
    pub origin: String,
    /// Why the last download of this page failed (feed pages are retried).
    pub capture_error: Option<String>,
    /// When an imported copy had its images and styles embedded.
    pub localized_at: Option<String>,
    /// The thread this page belongs to, if it was saved as part of one.
    #[serde(default)]
    pub thread_id: Option<i64>,
    #[serde(default)]
    pub thread_title: Option<String>,
    /// 1-based page number inside the thread.
    #[serde(default)]
    pub thread_page: Option<i64>,
    /// How many pages the thread has.
    #[serde(default)]
    pub thread_page_count: Option<i64>,
    /// complete | partial
    #[serde(default)]
    pub thread_status: Option<String>,
    #[serde(default)]
    pub thread_saved_at: Option<String>,
    /// Search snippet, only set on search results.
    pub snippet: Option<String>,
}

/// A multi-page thread saved as one item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Thread {
    pub id: i64,
    /// The address the save started from.
    pub url: String,
    pub title: Option<String>,
    pub saved_at: String,
    /// complete | partial
    pub status: String,
    /// Why the thread is partial (cap reached, a page failed), if it is.
    pub note: Option<String>,
    /// The pages in order.
    pub pages: Vec<Page>,
}

/// Fields for a new page row. Everything not listed starts empty/off.
#[derive(Debug, Clone, Default)]
pub struct NewPage {
    pub url: String,
    pub final_url: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub feed_id: Option<i64>,
    pub guid: Option<String>,
    pub published_at: Option<String>,
    pub saved_at: Option<String>,
    pub content_type: String,
    pub body_text: String,
    pub assets_inlined: i64,
    pub assets_failed: i64,
    pub origin: String,
    pub remote_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub pages: i64,
    pub feeds: i64,
    pub bytes: i64,
    pub wayback_done: i64,
    pub ipfs_done: i64,
}

const PAGE_COLUMNS: &str = "p.id, p.uid, p.url, p.final_url, p.title, p.author, p.feed_id, f.name, p.guid, \
    p.published_at, p.saved_at, p.content_type, p.local_path, p.size_bytes, p.sha256, p.assets_inlined, \
    p.assets_failed, p.wayback_status, p.wayback_url, p.wayback_error, p.wayback_at, p.ipfs_status, p.cid, \
    p.pin_service, p.ipfs_error, p.ipfs_at, p.origin, p.capture_error, p.localized_at, \
    tp.thread_id, t.title, \
    CASE WHEN tp.thread_id IS NULL THEN NULL ELSE (SELECT COUNT(*) + 1 FROM thread_pages x WHERE x.thread_id = tp.thread_id \
        AND (x.position < tp.position OR (x.position = tp.position AND x.page_id < tp.page_id))) END, \
    CASE WHEN tp.thread_id IS NULL THEN NULL ELSE (SELECT COUNT(*) FROM thread_pages x WHERE x.thread_id = tp.thread_id) END, \
    t.status, t.saved_at";

/// Index of the snippet column that search queries add after PAGE_COLUMNS.
const SNIPPET_COL: usize = 35;

/// The joins every PAGE_COLUMNS query needs.
const PAGE_JOINS: &str = "LEFT JOIN feeds f ON f.id = p.feed_id \
    LEFT JOIN thread_pages tp ON tp.page_id = p.id LEFT JOIN threads t ON t.id = tp.thread_id";

fn page_from_row(r: &Row, snippet: Option<String>) -> rusqlite::Result<Page> {
    Ok(Page {
        id: r.get(0)?,
        uid: r.get(1)?,
        url: r.get(2)?,
        final_url: r.get(3)?,
        title: r.get(4)?,
        author: r.get(5)?,
        feed_id: r.get(6)?,
        feed_name: r.get(7)?,
        guid: r.get(8)?,
        published_at: r.get(9)?,
        saved_at: r.get(10)?,
        content_type: r.get(11)?,
        local_path: r.get(12)?,
        size_bytes: r.get(13)?,
        sha256: r.get(14)?,
        assets_inlined: r.get(15)?,
        assets_failed: r.get(16)?,
        wayback_status: r.get(17)?,
        wayback_url: r.get(18)?,
        wayback_error: r.get(19)?,
        wayback_at: r.get(20)?,
        ipfs_status: r.get(21)?,
        cid: r.get(22)?,
        pin_service: r.get(23)?,
        ipfs_error: r.get(24)?,
        ipfs_at: r.get(25)?,
        origin: r.get(26)?,
        capture_error: r.get(27)?,
        localized_at: r.get(28)?,
        thread_id: r.get(29)?,
        thread_title: r.get(30)?,
        thread_page: r.get(31)?,
        thread_page_count: r.get(32)?,
        thread_status: r.get(33)?,
        thread_saved_at: r.get(34)?,
        snippet,
    })
}

impl Store {
    /// Opens (creating if needed) the archive in `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("pages"))
            .with_context(|| format!("creating {}", root.display()))?;
        let conn = Connection::open(root.join("persistwise.sqlite3"))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
        )?;
        migrate(&conn)?;
        Ok(Self {
            root,
            conn: Mutex::new(conn),
        })
    }

    /// The archive folder to open: `PERSISTWISE_DATA_DIR` if set, else the
    /// folder the user chose in Settings, else the default
    /// ([`crate::location::default_dir`]). See [`crate::location`].
    pub fn default_root() -> Result<PathBuf> {
        Ok(crate::location::Locator::from_env()?.resolve()?.path)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Absolute path of a page's saved file, refusing anything that would
    /// leave the `pages/` folder.
    pub fn file_path(&self, page: &Page) -> Option<PathBuf> {
        let rel = page.local_path.as_deref()?;
        let rel_path = Path::new(rel);
        if rel_path.is_absolute()
            || rel_path
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            || !rel.starts_with("pages/")
        {
            return None;
        }
        let p = self.root.join(rel_path);
        p.is_file().then_some(p)
    }

    /// Writes a saved copy for `uid` and returns (relative path, size, sha256).
    pub fn write_file(&self, uid: &str, ext: &str, bytes: &[u8]) -> Result<(String, i64, String)> {
        if uid.len() < 2 || !uid.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("bad page id");
        }
        let ext = if ext.chars().all(|c| c.is_ascii_alphanumeric()) && !ext.is_empty() {
            ext
        } else {
            "bin"
        };
        let rel = format!("pages/{}/{}.{}", &uid[..2], uid, ext);
        let abs = self.root.join(&rel);
        std::fs::create_dir_all(abs.parent().unwrap())?;
        let tmp = abs.with_extension(format!("{ext}.part"));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &abs)?;
        Ok((rel, bytes.len() as i64, util::sha256_hex(bytes)))
    }

    /// Inserts a page row with its saved file and search text.
    pub fn insert_page(
        &self,
        new: NewPage,
        file: Option<(String, i64, String)>,
        uid: &str,
    ) -> Result<Page> {
        let conn = self.conn();
        let saved_at = new.saved_at.clone().unwrap_or_else(util::now);
        let (path, size, sha) = match file {
            Some((p, s, h)) => (Some(p), Some(s), Some(h)),
            None => (None, None, None),
        };
        conn.execute(
            "INSERT INTO pages (uid, url, final_url, title, author, feed_id, guid, published_at, saved_at, \
             content_type, local_path, size_bytes, sha256, assets_inlined, assets_failed, origin, remote_key) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                uid, new.url, new.final_url, new.title, new.author, new.feed_id, new.guid, new.published_at,
                saved_at, new.content_type, path, size, sha, new.assets_inlined, new.assets_failed,
                new.origin, new.remote_key
            ],
        )?;
        let id = conn.last_insert_rowid();
        index_page(&conn, id, new.title.as_deref(), &new.url, &new.body_text)?;
        drop(conn);
        self.page(id)?
            .ok_or_else(|| anyhow!("page vanished after insert"))
    }

    pub fn page(&self, id: i64) -> Result<Option<Page>> {
        let conn = self.conn();
        let sql = format!("SELECT {PAGE_COLUMNS} FROM pages p {PAGE_JOINS} WHERE p.id = ?1");
        Ok(conn
            .query_row(&sql, [id], |r| page_from_row(r, None))
            .optional()?)
    }

    pub fn page_by_remote_key(&self, key: &str) -> Result<Option<Page>> {
        let conn = self.conn();
        let sql =
            format!("SELECT {PAGE_COLUMNS} FROM pages p {PAGE_JOINS} WHERE p.remote_key = ?1");
        Ok(conn
            .query_row(&sql, [key], |r| page_from_row(r, None))
            .optional()?)
    }

    /// Lists pages, newest first. With a query, runs a full-text search over
    /// title, URL, page text and feed name, best matches first.
    pub fn list_pages(
        &self,
        query: Option<&str>,
        feed_id: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Page>> {
        let conn = self.conn();
        let fts = query.and_then(fts_query);
        let mut out = Vec::new();
        match fts {
            Some(q) => {
                let sql = format!(
                    "SELECT {PAGE_COLUMNS}, snippet(pages_fts, 2, char(57344), char(57345), '…', 16) \
                     FROM pages_fts JOIN pages p ON p.id = pages_fts.rowid {PAGE_JOINS} \
                     WHERE pages_fts MATCH ?1 AND (?2 IS NULL OR p.feed_id = ?2) \
                     ORDER BY bm25(pages_fts, 10.0, 4.0, 1.0, 2.0), p.saved_at DESC LIMIT ?3 OFFSET ?4"
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(params![q, feed_id, limit, offset], |r| {
                    let snip: Option<String> = r.get(SNIPPET_COL)?;
                    page_from_row(r, snip.filter(|s| !s.is_empty()))
                })?;
                for r in rows {
                    out.push(r?);
                }
            }
            None => {
                let sql = format!(
                    "SELECT {PAGE_COLUMNS} FROM pages p {PAGE_JOINS} \
                     WHERE (?1 IS NULL OR p.feed_id = ?1) \
                     AND (?1 IS NOT NULL OR tp.thread_id IS NULL OR NOT EXISTS (SELECT 1 FROM thread_pages x \
                          WHERE x.thread_id = tp.thread_id AND (x.position < tp.position OR (x.position = tp.position AND x.page_id < tp.page_id)))) \
                     ORDER BY COALESCE(t.saved_at, p.published_at, p.saved_at) DESC, p.id DESC LIMIT ?2 OFFSET ?3"
                );
                let mut stmt = conn.prepare(&sql)?;
                let rows =
                    stmt.query_map(params![feed_id, limit, offset], |r| page_from_row(r, None))?;
                for r in rows {
                    out.push(r?);
                }
            }
        }
        Ok(out)
    }

    pub fn delete_page(&self, id: i64) -> Result<()> {
        let page = self.page(id)?;
        let conn = self.conn();
        conn.execute("DELETE FROM pages_fts WHERE rowid = ?1", [id])?;
        conn.execute("DELETE FROM pages WHERE id = ?1", [id])?;
        // Membership goes with the page (ON DELETE CASCADE); drop emptied threads.
        conn.execute(
            "DELETE FROM threads WHERE id NOT IN (SELECT thread_id FROM thread_pages)",
            [],
        )?;
        drop(conn);
        if let Some(path) = page.as_ref().and_then(|p| self.file_path(p)) {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }

    pub fn set_wayback(
        &self,
        id: i64,
        status: &str,
        url: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE pages SET wayback_status = ?2, wayback_url = COALESCE(?3, wayback_url), wayback_error = ?4, wayback_at = ?5 WHERE id = ?1",
            params![id, status, url, error, util::now()],
        )?;
        Ok(())
    }

    pub fn set_ipfs(
        &self,
        id: i64,
        status: &str,
        cid: Option<&str>,
        service: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE pages SET ipfs_status = ?2, cid = COALESCE(?3, cid), pin_service = COALESCE(?4, pin_service), ipfs_error = ?5, ipfs_at = ?6 WHERE id = ?1",
            params![id, status, cid, service, error, util::now()],
        )?;
        Ok(())
    }

    /// Updates the metadata of a page that came from an import. Leaves the
    /// local file alone unless `file` is given.
    pub fn update_imported_page(
        &self,
        id: i64,
        new: &NewPage,
        file: Option<(String, i64, String)>,
    ) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE pages SET url=?2, title=?3, author=?4, feed_id=?5, guid=?6, published_at=?7, \
             saved_at=COALESCE(?8, saved_at), content_type=?9 WHERE id=?1",
            params![
                id,
                new.url,
                new.title,
                new.author,
                new.feed_id,
                new.guid,
                new.published_at,
                new.saved_at,
                new.content_type
            ],
        )?;
        if let Some((p, s, h)) = file {
            conn.execute(
                "UPDATE pages SET local_path=?2, size_bytes=?3, sha256=?4 WHERE id=?1",
                params![id, p, s, h],
            )?;
        }
        let body: String = if new.body_text.is_empty() {
            conn.query_row("SELECT body FROM pages_fts WHERE rowid = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or_default()
        } else {
            new.body_text.clone()
        };
        index_page(&conn, id, new.title.as_deref(), &new.url, &body)?;
        Ok(())
    }

    /// Restores Wayback/IPFS fields exactly as an import describes them.
    pub fn set_backup_fields(
        &self,
        id: i64,
        wayback: (&str, Option<&str>, Option<&str>, Option<&str>),
        ipfs: (&str, Option<&str>, Option<&str>, Option<&str>, Option<&str>),
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE pages SET wayback_status=?2, wayback_url=?3, wayback_error=?4, wayback_at=?5, \
             ipfs_status=?6, cid=?7, pin_service=?8, ipfs_error=?9, ipfs_at=?10 WHERE id=?1",
            params![
                id, wayback.0, wayback.1, wayback.2, wayback.3, ipfs.0, ipfs.1, ipfs.2, ipfs.3,
                ipfs.4
            ],
        )?;
        Ok(())
    }

    /// Inserts or updates a feed identified by `remote_key`. Returns (id, created).
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_feed(
        &self,
        remote_key: &str,
        url: &str,
        name: Option<&str>,
        kind: Option<&str>,
        status: Option<&str>,
        last_polled_at: Option<&str>,
        created_at: Option<&str>,
    ) -> Result<(i64, bool)> {
        let conn = self.conn();
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM feeds WHERE remote_key = ?1",
                [remote_key],
                |r| r.get(0),
            )
            .optional()?;
        // A feed already followed on this computer with the same URL is adopted.
        let existing = match existing {
            Some(id) => Some(id),
            None => {
                let by_url: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM feeds WHERE url = ?1 AND remote_key IS NULL",
                        [url],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(id) = by_url {
                    conn.execute(
                        "UPDATE feeds SET remote_key = ?2 WHERE id = ?1",
                        params![id, remote_key],
                    )?;
                }
                by_url
            }
        };
        if let Some(id) = existing {
            conn.execute(
                "UPDATE feeds SET url=?2, name=?3, kind=?4, status=?5, last_polled_at=?6 WHERE id=?1",
                params![id, url, name, kind, status, last_polled_at],
            )?;
            // Keep feed names in the search index current.
            conn.execute(
                "UPDATE pages_fts SET feed = ?2 WHERE rowid IN (SELECT id FROM pages WHERE feed_id = ?1)",
                params![id, name.unwrap_or(url)],
            )?;
            return Ok((id, false));
        }
        conn.execute(
            "INSERT INTO feeds (url, name, kind, status, last_polled_at, created_at, origin, remote_key) \
             VALUES (?1,?2,?3,?4,?5,?6,'import',?7)",
            params![url, name, kind, status, last_polled_at, created_at.map(str::to_string).unwrap_or_else(util::now), remote_key],
        )?;
        Ok((conn.last_insert_rowid(), true))
    }

    pub fn feeds(&self) -> Result<Vec<Feed>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT f.id, f.url, f.name, f.kind, f.status, f.last_polled_at, f.created_at, f.origin, \
             (SELECT COUNT(*) FROM pages p WHERE p.feed_id = f.id), f.last_error FROM feeds f ORDER BY COALESCE(f.name, f.url)",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Feed {
                id: r.get(0)?,
                url: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
                status: r.get(4)?,
                last_polled_at: r.get(5)?,
                created_at: r.get(6)?,
                origin: r.get(7)?,
                page_count: r.get(8)?,
                last_error: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn feed(&self, id: i64) -> Result<Option<Feed>> {
        Ok(self.feeds()?.into_iter().find(|f| f.id == id))
    }

    /// Adds a feed the user subscribed to on this computer. Returns the
    /// existing feed if that URL is already there.
    pub fn add_feed(
        &self,
        url: &str,
        name: Option<&str>,
        kind: Option<&str>,
    ) -> Result<(i64, bool)> {
        let conn = self.conn();
        if let Some(id) = conn
            .query_row("SELECT id FROM feeds WHERE url = ?1", [url], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
        {
            return Ok((id, false));
        }
        conn.execute(
            "INSERT INTO feeds (url, name, kind, status, created_at, origin) VALUES (?1, ?2, ?3, 'active', ?4, 'desktop')",
            params![url, name, kind, util::now()],
        )?;
        Ok((conn.last_insert_rowid(), true))
    }

    /// Removes a feed. Its saved pages stay in the archive.
    pub fn delete_feed(&self, id: i64) -> Result<()> {
        let conn = self.conn();
        conn.execute("UPDATE pages_fts SET feed = '' WHERE rowid IN (SELECT id FROM pages WHERE feed_id = ?1)", [id])?;
        conn.execute("UPDATE pages SET feed_id = NULL WHERE feed_id = ?1", [id])?;
        conn.execute("DELETE FROM feeds WHERE id = ?1", [id])?;
        Ok(())
    }

    /// active | paused | errored, plus the last poll time and error.
    pub fn set_feed_status(
        &self,
        id: i64,
        status: &str,
        error: Option<&str>,
        polled: bool,
    ) -> Result<()> {
        let conn = self.conn();
        if polled {
            conn.execute(
                "UPDATE feeds SET status = ?2, last_error = ?3, last_polled_at = ?4 WHERE id = ?1",
                params![id, status, error, util::now()],
            )?;
        } else {
            conn.execute(
                "UPDATE feeds SET status = ?2, last_error = ?3 WHERE id = ?1",
                params![id, status, error],
            )?;
        }
        Ok(())
    }

    pub fn set_feed_name(&self, id: i64, name: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE feeds SET name = ?2 WHERE id = ?1 AND (name IS NULL OR name = '')",
            params![id, name],
        )?;
        Ok(())
    }

    pub fn page_by_feed_guid(&self, feed_id: i64, guid: &str) -> Result<Option<Page>> {
        let conn = self.conn();
        let sql = format!(
            "SELECT {PAGE_COLUMNS} FROM pages p {PAGE_JOINS} WHERE p.feed_id = ?1 AND p.guid = ?2"
        );
        Ok(conn
            .query_row(&sql, params![feed_id, guid], |r| page_from_row(r, None))
            .optional()?)
    }

    /// Links an existing page to a web-export record so later imports update it.
    pub fn set_remote_key(&self, id: i64, key: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE pages SET remote_key = ?2 WHERE id = ?1",
            params![id, key],
        )?;
        Ok(())
    }

    /// Pages of a feed matching a WHERE fragment over `p` (internal use).
    fn feed_pages_where(&self, feed_id: i64, cond: &str) -> Result<Vec<Page>> {
        let conn = self.conn();
        let sql = format!(
            "SELECT {PAGE_COLUMNS} FROM pages p {PAGE_JOINS} WHERE p.feed_id = ?1 AND ({cond}) ORDER BY p.id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([feed_id], |r| page_from_row(r, None))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Feed pages whose download failed and should be tried again.
    pub fn pages_needing_capture(&self, feed_id: i64, max_attempts: i64) -> Result<Vec<Page>> {
        self.feed_pages_where(
            feed_id,
            &format!("p.local_path IS NULL AND p.capture_attempts < {max_attempts}"),
        )
    }

    /// Feed pages without a Wayback snapshot whose last try (if any) is older
    /// than `retry_after_minutes`.
    pub fn pages_needing_wayback(
        &self,
        feed_id: i64,
        retry_after_minutes: i64,
    ) -> Result<Vec<Page>> {
        self.feed_pages_where(
            feed_id,
            &format!(
                "p.wayback_url IS NULL AND p.wayback_status != 'pending' AND (p.wayback_at IS NULL OR p.wayback_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-{retry_after_minutes} minutes'))"
            ),
        )
    }

    /// Feed pages with a local copy but no CID whose last try (if any) is
    /// older than `retry_after_minutes`.
    pub fn pages_needing_ipfs(&self, feed_id: i64, retry_after_minutes: i64) -> Result<Vec<Page>> {
        self.feed_pages_where(
            feed_id,
            &format!(
                "p.cid IS NULL AND p.local_path IS NOT NULL AND p.ipfs_status != 'pending' AND (p.ipfs_at IS NULL OR p.ipfs_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-{retry_after_minutes} minutes'))"
            ),
        )
    }

    pub fn record_capture_failure(&self, id: i64, error: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE pages SET capture_error = ?2, capture_attempts = capture_attempts + 1 WHERE id = ?1",
            params![id, error],
        )?;
        Ok(())
    }

    /// Attaches a successful (re)download to an existing page row.
    #[allow(clippy::too_many_arguments)]
    pub fn attach_capture(
        &self,
        id: i64,
        file: (String, i64, String),
        content_type: &str,
        final_url: Option<&str>,
        title: Option<&str>,
        body_text: &str,
        assets: (i64, i64),
    ) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE pages SET local_path = ?2, size_bytes = ?3, sha256 = ?4, content_type = ?5, final_url = COALESCE(?6, final_url), \
             title = COALESCE(title, ?7), assets_inlined = ?8, assets_failed = ?9, capture_error = NULL, \
             capture_attempts = capture_attempts + 1, saved_at = ?10 WHERE id = ?1",
            params![id, file.0, file.1, file.2, content_type, final_url, title, assets.0, assets.1, util::now()],
        )?;
        let (t, url): (Option<String>, String) =
            conn.query_row("SELECT title, url FROM pages WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        index_page(&conn, id, t.as_deref(), &url, body_text)?;
        Ok(())
    }

    /// Imported HTML copies whose images/styles still point at the web.
    pub fn pages_needing_localize(&self, limit: i64) -> Result<Vec<Page>> {
        let conn = self.conn();
        let sql = format!(
            "SELECT {PAGE_COLUMNS} FROM pages p {PAGE_JOINS} \
             WHERE p.origin = 'import' AND p.local_path IS NOT NULL AND p.content_type = 'text/html' \
             AND p.localized_at IS NULL ORDER BY p.id LIMIT ?1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([limit], |r| page_from_row(r, None))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces a page's file with its localized version. `saved_at` is kept.
    pub fn set_localized(
        &self,
        id: i64,
        file: Option<(String, i64, String)>,
        assets: (i64, i64),
    ) -> Result<()> {
        let conn = self.conn();
        if let Some((p, sz, sha)) = file {
            conn.execute(
                "UPDATE pages SET local_path = ?2, size_bytes = ?3, sha256 = ?4 WHERE id = ?1",
                params![id, p, sz, sha],
            )?;
        }
        conn.execute(
            "UPDATE pages SET assets_inlined = ?2, assets_failed = ?3, localized_at = ?4 WHERE id = ?1",
            params![id, assets.0, assets.1, util::now()],
        )?;
        Ok(())
    }

    /// The newest page saved from `url` (as typed or after redirects) that
    /// has a local copy. Used to avoid saving the same thread page twice.
    pub fn page_by_url(&self, urls: &[&str]) -> Result<Option<Page>> {
        let conn = self.conn();
        let sql = format!(
            "SELECT {PAGE_COLUMNS} FROM pages p {PAGE_JOINS} \
             WHERE (p.url = ?1 OR p.final_url = ?1) AND p.local_path IS NOT NULL ORDER BY p.id DESC LIMIT 1"
        );
        for u in urls {
            if let Some(p) = conn
                .query_row(&sql, [u], |r| page_from_row(r, None))
                .optional()?
            {
                return Ok(Some(p));
            }
        }
        Ok(None)
    }

    /// The thread a page belongs to and its position there.
    pub fn thread_of_page(&self, page_id: i64) -> Result<Option<(i64, i64)>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT thread_id, position FROM thread_pages WHERE page_id = ?1",
                [page_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn create_thread(
        &self,
        url: &str,
        title: Option<&str>,
        status: &str,
        note: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO threads (url, title, saved_at, status, note) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![url, title, util::now(), status, note],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Records the outcome of the latest save of a thread.
    pub fn update_thread(&self, id: i64, status: &str, note: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE threads SET status = ?2, note = ?3, saved_at = ?4 WHERE id = ?1",
            params![id, status, note, util::now()],
        )?;
        Ok(())
    }

    /// Puts a page at `position` in a thread, taking it out of any other
    /// thread it was in (threads left empty are removed).
    pub fn set_thread_page(&self, thread_id: i64, page_id: i64, position: i64) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO thread_pages (page_id, thread_id, position) VALUES (?1, ?2, ?3) \
             ON CONFLICT(page_id) DO UPDATE SET thread_id = excluded.thread_id, position = excluded.position",
            params![page_id, thread_id, position],
        )?;
        conn.execute(
            "DELETE FROM threads WHERE id NOT IN (SELECT thread_id FROM thread_pages)",
            [],
        )?;
        Ok(())
    }

    /// A thread with its pages in order.
    pub fn thread(&self, id: i64) -> Result<Option<Thread>> {
        let conn = self.conn();
        let head = conn
            .query_row(
                "SELECT id, url, title, saved_at, status, note FROM threads WHERE id = ?1",
                [id],
                |r| {
                    Ok(Thread {
                        id: r.get(0)?,
                        url: r.get(1)?,
                        title: r.get(2)?,
                        saved_at: r.get(3)?,
                        status: r.get(4)?,
                        note: r.get(5)?,
                        pages: Vec::new(),
                    })
                },
            )
            .optional()?;
        let Some(mut thread) = head else {
            return Ok(None);
        };
        let sql = format!(
            "SELECT {PAGE_COLUMNS} FROM pages p {PAGE_JOINS} WHERE tp.thread_id = ?1 ORDER BY tp.position, tp.page_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([id], |r| page_from_row(r, None))?;
        thread.pages = rows.collect::<rusqlite::Result<_>>()?;
        Ok(Some(thread))
    }

    /// Deletes a thread and every page in it.
    pub fn delete_thread(&self, id: i64) -> Result<()> {
        let ids: Vec<i64> = {
            let conn = self.conn();
            let mut stmt = conn.prepare("SELECT page_id FROM thread_pages WHERE thread_id = ?1")?;
            let rows = stmt.query_map([id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for pid in ids {
            self.delete_page(pid)?;
        }
        self.conn()
            .execute("DELETE FROM threads WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Writes a consistent copy of the database to `dest` (which must not
    /// exist yet), even while the archive is in use.
    pub fn copy_database_to(&self, dest: &Path) -> Result<()> {
        let d = dest
            .to_str()
            .ok_or_else(|| anyhow!("the new folder's path is not valid Unicode"))?;
        self.conn()
            .execute("VACUUM INTO ?1", [d])
            .with_context(|| format!("copying the database to {}", dest.display()))?;
        Ok(())
    }

    /// (relative path, size) of every saved file the archive knows about.
    pub fn local_files(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT local_path, COALESCE(size_bytes, -1) FROM pages WHERE local_path IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn stats(&self) -> Result<Stats> {
        let conn = self.conn();
        Ok(conn.query_row(
            "SELECT (SELECT COUNT(*) FROM pages), (SELECT COUNT(*) FROM feeds), \
             (SELECT COALESCE(SUM(size_bytes),0) FROM pages), \
             (SELECT COUNT(*) FROM pages WHERE wayback_url IS NOT NULL), \
             (SELECT COUNT(*) FROM pages WHERE cid IS NOT NULL)",
            [],
            |r| {
                Ok(Stats {
                    pages: r.get(0)?,
                    feeds: r.get(1)?,
                    bytes: r.get(2)?,
                    wayback_done: r.get(3)?,
                    ipfs_done: r.get(4)?,
                })
            },
        )?)
    }
}

fn index_page(
    conn: &Connection,
    id: i64,
    title: Option<&str>,
    url: &str,
    body: &str,
) -> Result<()> {
    let feed: Option<String> = conn
        .query_row(
            "SELECT COALESCE(f.name, f.url) FROM pages p JOIN feeds f ON f.id = p.feed_id WHERE p.id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    conn.execute("DELETE FROM pages_fts WHERE rowid = ?1", [id])?;
    conn.execute(
        "INSERT INTO pages_fts (rowid, title, url, body, feed) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, title.unwrap_or(""), url, body, feed.unwrap_or_default()],
    )?;
    Ok(())
}

/// Turns what a person types into a safe FTS5 query: every word must match,
/// as a prefix, and FTS syntax characters are treated as plain text.
pub fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .map(|t| t.replace('"', ""))
        .filter(|t| t.chars().any(|c| c.is_alphanumeric()))
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        bail!("this archive was made by a newer Persistwise Desktop (schema {version}); please update the app");
    }
    if version < 1 {
        conn.execute_batch(
            r#"
            BEGIN;
            CREATE TABLE feeds (
                id INTEGER PRIMARY KEY,
                url TEXT NOT NULL,
                name TEXT,
                kind TEXT,
                status TEXT,
                last_polled_at TEXT,
                created_at TEXT NOT NULL,
                origin TEXT NOT NULL DEFAULT 'desktop',
                remote_key TEXT UNIQUE
            );
            CREATE TABLE pages (
                id INTEGER PRIMARY KEY,
                uid TEXT NOT NULL UNIQUE,
                url TEXT NOT NULL,
                final_url TEXT,
                title TEXT,
                author TEXT,
                feed_id INTEGER REFERENCES feeds(id) ON DELETE SET NULL,
                guid TEXT,
                published_at TEXT,
                saved_at TEXT NOT NULL,
                content_type TEXT NOT NULL DEFAULT 'text/html',
                local_path TEXT,
                size_bytes INTEGER,
                sha256 TEXT,
                assets_inlined INTEGER NOT NULL DEFAULT 0,
                assets_failed INTEGER NOT NULL DEFAULT 0,
                wayback_status TEXT NOT NULL DEFAULT 'off',
                wayback_url TEXT,
                wayback_error TEXT,
                wayback_at TEXT,
                ipfs_status TEXT NOT NULL DEFAULT 'off',
                cid TEXT,
                pin_service TEXT,
                ipfs_error TEXT,
                ipfs_at TEXT,
                origin TEXT NOT NULL DEFAULT 'desktop',
                remote_key TEXT UNIQUE
            );
            CREATE INDEX pages_saved_at ON pages(saved_at);
            CREATE INDEX pages_feed ON pages(feed_id);
            CREATE VIRTUAL TABLE pages_fts USING fts5(title, url, body, feed, tokenize = 'unicode61 remove_diacritics 2');
            PRAGMA user_version = 1;
            COMMIT;
            "#,
        )?;
    }
    if version < 2 {
        // v2: local feed polling. Feeds remember their last error and can be
        // paused; feed pages whose download failed are retried on later polls.
        conn.execute_batch(
            r#"
            BEGIN;
            ALTER TABLE feeds ADD COLUMN last_error TEXT;
            ALTER TABLE pages ADD COLUMN capture_error TEXT;
            ALTER TABLE pages ADD COLUMN capture_attempts INTEGER NOT NULL DEFAULT 0;
            CREATE UNIQUE INDEX pages_feed_guid ON pages(feed_id, guid) WHERE feed_id IS NOT NULL AND guid IS NOT NULL;
            PRAGMA user_version = 2;
            COMMIT;
            "#,
        )?;
    }
    if version < 3 {
        // v3: imported copies get their images and styles embedded once.
        conn.execute_batch(
            r#"
            BEGIN;
            ALTER TABLE pages ADD COLUMN localized_at TEXT;
            PRAGMA user_version = 3;
            COMMIT;
            "#,
        )?;
    }
    if version < 4 {
        // v4: multi-page threads saved as one item.
        conn.execute_batch(
            r#"
            BEGIN;
            CREATE TABLE threads (
                id INTEGER PRIMARY KEY,
                url TEXT NOT NULL,
                title TEXT,
                saved_at TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'complete',
                note TEXT
            );
            CREATE TABLE thread_pages (
                page_id INTEGER PRIMARY KEY REFERENCES pages(id) ON DELETE CASCADE,
                thread_id INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                position INTEGER NOT NULL
            );
            CREATE INDEX thread_pages_thread ON thread_pages(thread_id, position);
            PRAGMA user_version = 4;
            COMMIT;
            "#,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(store: &Store, url: &str, title: &str, body: &str) -> Page {
        let uid = util::new_uid();
        let file = store.write_file(&uid, "html", body.as_bytes()).unwrap();
        store
            .insert_page(
                NewPage {
                    url: url.into(),
                    title: Some(title.into()),
                    content_type: "text/html".into(),
                    body_text: body.into(),
                    origin: "desktop".into(),
                    ..Default::default()
                },
                Some(file),
                &uid,
            )
            .unwrap()
    }

    #[test]
    fn insert_list_search_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let a = page(
            &store,
            "https://a.test/one",
            "Gardening notes",
            "Tomatoes like warm soil and steady water.",
        );
        let b = page(
            &store,
            "https://b.test/two",
            "Bread",
            "Sourdough starter needs flour and patience.",
        );
        assert_eq!(store.list_pages(None, None, 50, 0).unwrap().len(), 2);

        let hits = store.list_pages(Some("tomat"), None, 50, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, a.id);
        let snip = hits[0].snippet.as_deref().unwrap();
        assert!(snip.contains(HIT_START) && snip.contains(HIT_END), "{snip}");

        // FTS syntax in user input is treated as text, not an error.
        assert!(store
            .list_pages(Some("\"flour\" AND (NEAR"), None, 50, 0)
            .is_ok());
        assert_eq!(
            store
                .list_pages(Some("sourdough patience"), None, 50, 0)
                .unwrap()[0]
                .id,
            b.id
        );
        assert_eq!(
            store.list_pages(Some("b.test"), None, 50, 0).unwrap().len(),
            1
        );

        let path = store.file_path(&a).unwrap();
        assert!(path.exists());
        store.delete_page(a.id).unwrap();
        assert!(!path.exists());
        assert!(store
            .list_pages(Some("tomatoes"), None, 50, 0)
            .unwrap()
            .is_empty());
        assert_eq!(store.stats().unwrap().pages, 1);
    }

    #[test]
    fn file_path_refuses_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let mut p = page(&store, "https://a.test/", "t", "b");
        p.local_path = Some("pages/../persistwise.sqlite3".into());
        assert!(store.file_path(&p).is_none());
        p.local_path = Some("/etc/passwd".into());
        assert!(store.file_path(&p).is_none());
    }

    #[test]
    fn reopening_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path()).unwrap();
            page(&store, "https://a.test/", "Kept", "still here");
        }
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(
            store.list_pages(Some("still"), None, 10, 0).unwrap().len(),
            1
        );
    }
}
