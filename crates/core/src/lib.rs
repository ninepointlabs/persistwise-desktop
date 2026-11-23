//! Persistwise Desktop core: a personal web archive that lives entirely on
//! your machine. No Nine Point Labs servers are involved in anything here.
//!
//! * [`capture`]: save a full, self-contained copy of a page.
//! * [`store`]: the local SQLite archive and its full-text search.
//! * [`wayback`]: optional backup through the public Save Page Now.
//! * [`ipfs`]: optional pinning through the user's own node or key.
//! * [`import`]: bring in a Persistwise web account export.

pub mod capture;
pub mod feeds;
pub mod http;
pub mod import;
pub mod ipfs;
pub mod settings;
pub mod store;
pub mod text;
pub mod util;
pub mod wayback;

use anyhow::{anyhow, Result};
use store::{NewPage, Page, Store};

/// Saves `url` into the archive. Only the local copy happens here; the
/// optional Wayback and IPFS steps are separate ([`backup_wayback`],
/// [`backup_ipfs`]) so a slow or failing backup can never lose a save.
pub fn save_page(store: &Store, url: &str, opts: &capture::CaptureOptions) -> Result<Page> {
    let cap = capture::capture(url, opts)?;
    let uid = util::new_uid();
    let file = store.write_file(&uid, cap.ext, &cap.bytes)?;
    store.insert_page(
        NewPage {
            url: url.trim().to_string(),
            final_url: (cap.final_url != url.trim()).then(|| cap.final_url.clone()),
            title: cap.title,
            content_type: cap.content_type,
            body_text: cap.text,
            assets_inlined: cap.assets_inlined as i64,
            assets_failed: cap.assets_failed as i64,
            origin: "desktop".into(),
            ..Default::default()
        },
        Some(file),
        &uid,
    )
}

/// Sends a saved page's URL to Save Page Now and records the outcome.
pub fn backup_wayback(store: &Store, id: i64, wb: &wayback::Wayback) -> Result<Page> {
    let page = store.page(id)?.ok_or_else(|| anyhow!("no such page"))?;
    store.set_wayback(id, "pending", None, None)?;
    match wb.save(&page.url) {
        Ok(snapshot) => store.set_wayback(id, "done", Some(&snapshot), None)?,
        Err(e) => store.set_wayback(id, "failed", None, Some(&format!("{e:#}")))?,
    }
    store.page(id)?.ok_or_else(|| anyhow!("no such page"))
}

/// Pins a saved page's file to IPFS if the user set IPFS up. When IPFS is
/// off this does nothing and returns the page unchanged.
pub fn backup_ipfs(store: &Store, id: i64) -> Result<Page> {
    let page = store.page(id)?.ok_or_else(|| anyhow!("no such page"))?;
    let s = settings::load(store.root());
    let secrets = settings::load_secrets(store.root());
    if !ipfs::is_configured(&s.ipfs, &secrets) {
        return Ok(page);
    }
    let Some(path) = store.file_path(&page) else {
        store.set_ipfs(id, "failed", None, None, Some("the saved copy is missing"))?;
        return store.page(id)?.ok_or_else(|| anyhow!("no such page"));
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("page.html")
        .to_string();
    store.set_ipfs(id, "pending", None, None, None)?;
    match ipfs::pin_file(&s.ipfs, &secrets, &ipfs::Endpoints::default(), &path, &name) {
        Ok(p) => store.set_ipfs(id, "done", Some(&p.cid), Some(p.service), None)?,
        Err(e) => store.set_ipfs(id, "failed", None, None, Some(&format!("{e:#}")))?,
    }
    store.page(id)?.ok_or_else(|| anyhow!("no such page"))
}
