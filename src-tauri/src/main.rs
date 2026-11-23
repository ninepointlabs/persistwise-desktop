// Persistwise Desktop: the window around persistwise-core. Everything runs
// on this machine; the only network traffic is fetching the pages you save
// and the optional backups you turn on.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod background;
mod hostenv;
mod poller;
mod theme;

use persistwise_core::capture::CaptureOptions;
use persistwise_core::settings::{self, Secrets, Settings};
use persistwise_core::store::{Feed, Page, Stats, Store};
use persistwise_core::wayback::Wayback;
use persistwise_core::{import, ipfs, util};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::http::{header, Response, StatusCode};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

struct AppState {
    store: Arc<Store>,
}

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Runs blocking archive work off the UI thread.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(err)?
        .map_err(|e| format!("{e:#}"))
}

#[derive(Serialize)]
struct AppInfo {
    version: &'static str,
    data_dir: String,
    platform: &'static str,
    stats: Stats,
    omarchy: bool,
}

#[tauri::command]
fn app_info(state: State<AppState>) -> CmdResult<AppInfo> {
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        data_dir: state.store.root().display().to_string(),
        platform: std::env::consts::OS,
        stats: state.store.stats().map_err(err)?,
        omarchy: theme::omarchy_theme_dir().is_some(),
    })
}

#[tauri::command]
fn list_pages(
    state: State<AppState>,
    query: Option<String>,
    feed_id: Option<i64>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> CmdResult<Vec<Page>> {
    state
        .store
        .list_pages(
            query.as_deref().filter(|q| !q.trim().is_empty()),
            feed_id,
            limit.unwrap_or(100).clamp(1, 500),
            offset.unwrap_or(0).max(0),
        )
        .map_err(err)
}

#[tauri::command]
fn get_page(state: State<AppState>, id: i64) -> CmdResult<Option<Page>> {
    state.store.page(id).map_err(err)
}

#[tauri::command]
fn list_feeds(state: State<AppState>) -> CmdResult<Vec<Feed>> {
    state.store.feeds().map_err(err)
}

#[tauri::command]
async fn save_page(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    wayback: bool,
) -> CmdResult<Page> {
    let store = state.store.clone();
    let page =
        blocking(move || persistwise_core::save_page(&store, &url, &CaptureOptions::default()))
            .await?;
    let s = settings::load(state.store.root());
    if wayback {
        spawn_wayback(app.clone(), state.store.clone(), page.id);
    }
    if s.ipfs.pin_on_save
        && ipfs::is_configured(&s.ipfs, &settings::load_secrets(state.store.root()))
    {
        spawn_ipfs(app, state.store.clone(), page.id);
    }
    Ok(page)
}

fn spawn_wayback(app: AppHandle, store: Arc<Store>, id: i64) {
    let _ = store.set_wayback(id, "pending", None, None);
    std::thread::spawn(move || {
        let result = persistwise_core::backup_wayback(&store, id, &Wayback::default());
        if let Ok(page) = result {
            let _ = app.emit("page-updated", page);
        }
    });
}

fn spawn_ipfs(app: AppHandle, store: Arc<Store>, id: i64) {
    let _ = store.set_ipfs(id, "pending", None, None, None);
    std::thread::spawn(move || {
        if let Ok(page) = persistwise_core::backup_ipfs(&store, id) {
            let _ = app.emit("page-updated", page);
        }
    });
}

#[tauri::command]
fn send_to_wayback(app: AppHandle, state: State<AppState>, id: i64) -> CmdResult<()> {
    state.store.page(id).map_err(err)?.ok_or("no such page")?;
    spawn_wayback(app, state.store.clone(), id);
    Ok(())
}

#[tauri::command]
fn pin_to_ipfs(app: AppHandle, state: State<AppState>, id: i64) -> CmdResult<()> {
    let s = settings::load(state.store.root());
    if !ipfs::is_configured(&s.ipfs, &settings::load_secrets(state.store.root())) {
        return Err("IPFS is not set up. You can set it up in Settings.".into());
    }
    spawn_ipfs(app, state.store.clone(), id);
    Ok(())
}

#[tauri::command]
fn delete_page(state: State<AppState>, id: i64) -> CmdResult<()> {
    state.store.delete_page(id).map_err(err)
}

// ---------- feeds ----------

#[tauri::command]
async fn add_feed(
    state: State<'_, AppState>,
    poller: State<'_, poller::Poller>,
    url: String,
) -> CmdResult<Feed> {
    let store = state.store.clone();
    let (feed, _created) =
        blocking(move || persistwise_core::feeds::subscribe(&store, &url)).await?;
    poller.check_now(Some(feed.id));
    Ok(feed)
}

/// Embeds images/styles into one imported copy now.
#[tauri::command]
async fn localize_page(state: State<'_, AppState>, id: i64) -> CmdResult<Page> {
    let store = state.store.clone();
    blocking(move || persistwise_core::localize_page(&store, id, &CaptureOptions::default())).await
}

#[tauri::command]
fn check_feeds(poller: State<poller::Poller>, feed_id: Option<i64>) {
    poller.check_now(feed_id);
}

#[tauri::command]
fn polling_now(poller: State<poller::Poller>) -> bool {
    poller.running.load(std::sync::atomic::Ordering::SeqCst)
}

#[tauri::command]
fn set_feed_paused(state: State<AppState>, id: i64, paused: bool) -> CmdResult<()> {
    state
        .store
        .set_feed_status(id, if paused { "paused" } else { "active" }, None, false)
        .map_err(err)
}

#[tauri::command]
fn remove_feed(state: State<AppState>, id: i64) -> CmdResult<()> {
    state.store.delete_feed(id).map_err(err)
}

#[tauri::command]
fn background_status(state: State<AppState>) -> background::Status {
    background::status(state.store.root())
}

#[tauri::command]
fn set_background(state: State<AppState>, enabled: bool) -> CmdResult<background::Status> {
    if enabled {
        let minutes = settings::load(state.store.root()).feeds.poll_minutes;
        background::enable(state.store.root(), if minutes == 0 { 15 } else { minutes })
    } else {
        background::disable(state.store.root())
    }
}

#[derive(Serialize)]
struct SettingsView {
    settings: Settings,
    kubo_token: Option<String>,
    pinata_jwt: Option<String>,
    gateway_default: String,
}

fn settings_view(store: &Store) -> SettingsView {
    let s = settings::load(store.root());
    let sec = settings::load_secrets(store.root());
    let mask = |v: Option<String>| v.filter(|v| !v.is_empty()).map(|v| util::mask_secret(&v));
    SettingsView {
        gateway_default: s.ipfs.gateway_or_default(),
        settings: s,
        kubo_token: mask(sec.kubo_token),
        pinata_jwt: mask(sec.pinata_jwt),
    }
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> SettingsView {
    settings_view(&state.store)
}

/// For each secret: absent = keep as is, "" = remove, anything else = replace.
#[derive(Deserialize)]
struct SecretsUpdate {
    kubo_token: Option<String>,
    pinata_jwt: Option<String>,
}

#[tauri::command]
fn save_settings(
    state: State<AppState>,
    settings: Settings,
    secrets: Option<SecretsUpdate>,
) -> CmdResult<SettingsView> {
    let root = state.store.root();
    settings::save(root, &settings).map_err(err)?;
    // Keep the background timer's schedule in step with the setting.
    if cfg!(target_os = "linux")
        && background::status(root).enabled
        && settings.feeds.poll_minutes > 0
    {
        let _ = background::enable(root, settings.feeds.poll_minutes);
    }
    if let Some(up) = secrets {
        let mut cur: Secrets = settings::load_secrets(root);
        let apply = |slot: &mut Option<String>, v: Option<String>| {
            if let Some(v) = v {
                let v = v.trim().to_string();
                *slot = if v.is_empty() { None } else { Some(v) };
            }
        };
        apply(&mut cur.kubo_token, up.kubo_token);
        apply(&mut cur.pinata_jwt, up.pinata_jwt);
        settings::save_secrets(root, &cur).map_err(err)?;
    }
    Ok(settings_view(&state.store))
}

#[tauri::command]
async fn test_ipfs(state: State<'_, AppState>) -> CmdResult<String> {
    let root = state.store.root().to_path_buf();
    blocking(move || {
        let s = settings::load(&root);
        ipfs::test_connection(
            &s.ipfs,
            &settings::load_secrets(&root),
            &ipfs::Endpoints::default(),
        )
    })
    .await
}

#[derive(Serialize)]
struct ImportResult {
    path: String,
    report: import::Report,
}

/// Lets the user pick an export (.zip or folder) and imports it.
/// Returns None if the picker was cancelled.
#[tauri::command]
async fn import_export(
    app: AppHandle,
    state: State<'_, AppState>,
    folder: bool,
) -> CmdResult<Option<ImportResult>> {
    let dialog = app.dialog().file().set_title("Choose a Persistwise export");
    let picked = if folder {
        dialog.blocking_pick_folder()
    } else {
        dialog
            .add_filter("Persistwise export", &["zip"])
            .blocking_pick_file()
    };
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(err)?;
    let store = state.store.clone();
    let shown = path.display().to_string();
    let report = blocking(move || import::import(&store, &path)).await?;
    Ok(Some(ImportResult {
        path: shown,
        report,
    }))
}

/// Opens a URL or file with the desktop's default app. On Linux this runs
/// xdg-open with the host environment restored (see hostenv.rs), falling
/// back to the opener plugin; elsewhere it uses the opener plugin.
fn open_target(app: &AppHandle, target: &str, is_url: bool) -> CmdResult<()> {
    #[cfg(target_os = "linux")]
    if hostenv::xdg_open(target).is_ok() {
        return Ok(());
    }
    if is_url {
        app.opener().open_url(target, None::<&str>).map_err(err)
    } else {
        app.opener().open_path(target, None::<&str>).map_err(err)
    }
}

#[tauri::command]
fn open_data_dir(app: AppHandle, state: State<AppState>) -> CmdResult<()> {
    open_target(&app, &state.store.root().display().to_string(), false)
}

/// Opens a page's saved file in the system's default app (for PDFs etc).
#[tauri::command]
fn open_saved_file(app: AppHandle, state: State<AppState>, id: i64) -> CmdResult<()> {
    let page = state.store.page(id).map_err(err)?.ok_or("no such page")?;
    let path = state
        .store
        .file_path(&page)
        .ok_or("the saved copy is missing")?;
    open_target(&app, &path.display().to_string(), false)
}

#[tauri::command]
fn open_external(app: AppHandle, url: String) -> CmdResult<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("only web links can be opened".into());
    }
    open_target(&app, &url, true)
}

#[tauri::command]
fn ipfs_link(state: State<AppState>, cid: String) -> String {
    ipfs::gateway_url(&settings::load(state.store.root()).ipfs, &cid)
}

#[tauri::command]
fn omarchy_theme() -> Option<theme::ThemeColors> {
    theme::read()
}

/// A saved page is someone else's HTML, shown with no scripts, no forms, no
/// plugins and an opaque origin. Inlined assets are data: URIs; remote
/// images/styles/fonts are allowed for copies whose assets were not inlined
/// (imports from the web app keep absolute links).
const SAVED_COPY_CSP: &str = "sandbox; default-src 'none'; img-src data: https: http:; style-src 'unsafe-inline' data: https: http:; \
    font-src data: https: http:; media-src data:; base-uri 'none'; form-action 'none'";

/// Origins of the app's own UI (Linux/macOS-style and Windows-style). Only
/// these may read saved copies with fetch(); iframes need no CORS.
const APP_ORIGINS: [&str; 3] = [
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

fn serve_saved_copy(store: &Store, request: &tauri::http::Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = request.uri().path();
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .filter(|o| APP_ORIGINS.contains(o))
        .map(str::to_string);
    let mut resp = serve_saved_copy_inner(store, path);
    if let Some(o) = origin {
        if let Ok(v) = header::HeaderValue::from_str(&o) {
            resp.headers_mut()
                .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
            resp.headers_mut().insert(
                header::ACCESS_CONTROL_EXPOSE_HEADERS,
                header::HeaderValue::from_static("Content-Security-Policy"),
            );
        }
    }
    resp
}

fn serve_saved_copy_inner(store: &Store, path: &str) -> Response<Vec<u8>> {
    let not_found = || {
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(b"Not found".to_vec())
            .unwrap()
    };
    let Ok(id) = path.trim_matches('/').parse::<i64>() else {
        return not_found();
    };
    let Ok(Some(page)) = store.page(id) else {
        return not_found();
    };
    let Some(file) = store.file_path(&page) else {
        return not_found();
    };
    let Ok(bytes) = std::fs::read(file) else {
        return not_found();
    };
    let ct = if page.content_type == "text/html" {
        "text/html; charset=utf-8".to_string()
    } else {
        page.content_type.clone()
    };
    Response::builder()
        .header(header::CONTENT_TYPE, ct)
        .header("Content-Security-Policy", SAVED_COPY_CSP)
        .header("X-Content-Type-Options", "nosniff")
        .header("Referrer-Policy", "no-referrer")
        .header(header::CACHE_CONTROL, "no-store")
        .body(bytes)
        .unwrap()
}

/// The desktop file id. Used as the Wayland app_id and X11 WM_CLASS so docks
/// and launchers group the window under the right icon.
pub const APP_ID: &str = "com.ninepointlabs.Persistwise";

const SMOKE_JS: &str = r#"
(async () => {
  const out = [];
  try {
    const { invoke } = window.__TAURI__.core;
    const info = await invoke("app_info");
    out.push("ipc ok, pages=" + info.stats.pages);
    const pages = await invoke("list_pages", { limit: 5 });
    out.push("list ok, " + pages.length + " rows, ui rendered " + document.querySelectorAll(".page").length + " cards");
    const withCopy = pages.find((p) => p.local_path);
    if (withCopy) {
      const url = info.platform === "windows" ? "http://pwcopy.localhost/" + withCopy.id : "pwcopy://localhost/" + withCopy.id;
      const r = await fetch(url);
      const body = await r.text();
      out.push("copy " + r.status + " csp=" + (r.headers.get("content-security-policy") || "").startsWith("sandbox") + " bytes=" + body.length);
      const missing = await fetch(url.replace(/\d+$/, "999999"));
      out.push("missing copy " + missing.status);
      const csp = (r.headers.get("content-security-policy") || "").startsWith("sandbox");
      if (r.status !== 200 || !csp || missing.status !== 404) throw new Error("saved copy protocol misbehaved");
    }
    // Feed polling through the real poller thread and its events.
    const feeds = await invoke("list_feeds");
    if (feeds.length) {
      const done = new Promise((resolve) => {
        window.__TAURI__.event.listen("poll-state", (e) => { if (!e.payload.running) resolve(e.payload); });
      });
      await invoke("check_feeds", { feedId: null });
      const st = await done;
      if (st.message) throw new Error("poll: " + st.message);
      out.push("poll ok, " + st.reports.length + " feeds, " + st.reports.reduce((a, r) => a + r.new_pages, 0) + " new, errors=" + st.reports.filter((r) => r.error).length);
    }
    await invoke("smoke_report", { ok: true, details: out.join("; ") });
  } catch (e) {
    await window.__TAURI__.core.invoke("smoke_report", { ok: false, details: out.join("; ") + " ERROR " + e });
  }
})();
"#;

#[tauri::command]
fn smoke_report(app: AppHandle, ok: bool, details: String) {
    println!(
        "smoke test {}: {details}",
        if ok { "passed" } else { "FAILED" }
    );
    app.exit(if ok { 0 } else { 5 });
}

/// Shows and focuses the main window (it starts hidden).
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[cfg(windows)]
mod tray {
    //! Tray icon so Persistwise can keep checking feeds with its window closed.
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    use tauri::Manager;

    pub fn install(app: &mut tauri::App) -> tauri::Result<()> {
        let open = MenuItem::with_id(app, "open", "Open Persistwise", true, None::<&str>)?;
        let check = MenuItem::with_id(app, "check", "Check feeds now", true, None::<&str>)?;
        let quit = MenuItem::with_id(app, "quit", "Quit Persistwise", true, None::<&str>)?;
        let menu = Menu::with_items(app, &[&open, &check, &quit])?;
        let mut tray = TrayIconBuilder::with_id("main")
            .tooltip("Persistwise")
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(|app, event| match event.id.as_ref() {
                "open" => super::show_main(app),
                "check" => app.state::<super::poller::Poller>().check_now(None),
                "quit" => app.exit(0),
                _ => {}
            })
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    super::show_main(tray.app_handle());
                }
            });
        if let Some(icon) = app.default_window_icon() {
            tray = tray.icon(icon.clone());
        }
        tray.build(app)?;
        Ok(())
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    {
        glib::set_prgname(Some(APP_ID));
        glib::set_application_name("Persistwise");
        // Wayland app_id and X11 WM_CLASS instance both come from this. (On
        // X11 GTK capitalises the class part; launchers match the instance.)
    }
    let root = match Store::default_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Persistwise: {e:#}");
            std::process::exit(1);
        }
    };
    if std::env::args().any(|a| a == "--version") {
        println!("persistwise-desktop {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let store = match Store::open(&root) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "Persistwise: could not open the archive in {}: {e:#}",
                root.display()
            );
            std::process::exit(1);
        }
    };
    let args: Vec<String> = std::env::args().collect();
    // `--import <export.zip|folder>`: import or re-sync a Persistwise web
    // export without opening a window (handy for a scheduled sync).
    if let Some(i) = args.iter().position(|a| a == "--import") {
        let Some(path) = args.get(i + 1) else {
            eprintln!("usage: persistwise-desktop --import <export.zip|folder>");
            std::process::exit(2);
        };
        match import::import(&store, std::path::Path::new(path)) {
            Ok(r) => {
                println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
                std::process::exit(if r.problems.is_empty() { 0 } else { 3 });
            }
            Err(e) => {
                eprintln!("import failed: {e:#}");
                std::process::exit(1);
            }
        }
    }
    // `--print-child-env`: show the environment a program started by the app
    // (xdg-open) would get. Used to check the AppImage does not leak its
    // bundled library paths.
    if args.iter().any(|a| a == "--print-child-env") {
        let out = hostenv::host_command("env").output();
        match out {
            Ok(o) => print!("{}", String::from_utf8_lossy(&o.stdout)),
            Err(e) => eprintln!("could not run env: {e}"),
        }
        return;
    }
    // `--self-test`: open the archive, print a summary and exit. Used by the
    // packaging smoke tests; no window, no network.
    // `--poll`: check feeds once, archive new posts, print a JSON report and
    // exit. This is what the background timer runs.
    if args.iter().any(|a| a == "--poll") {
        let opts = persistwise_core::feeds::PollOptions::from_settings(&store);
        match persistwise_core::feeds::poll_all(&store, None, &opts, &mut |_| {}) {
            Ok(reports) => {
                let localized =
                    persistwise_core::localize_imported(&store, 25, &opts.capture, &mut |_| {});
                println!(
                    "{}",
                    serde_json::to_string_pretty(&reports).unwrap_or_default()
                );
                if localized > 0 {
                    eprintln!("embedded images into {localized} imported page(s)");
                }
                return;
            }
            Err(e)
                if e.downcast_ref::<persistwise_core::feeds::AlreadyPolling>()
                    .is_some() =>
            {
                println!("Feeds are already being checked; nothing to do.");
                return;
            }
            Err(e) => {
                eprintln!("feed check failed: {e:#}");
                std::process::exit(1);
            }
        }
    }
    if args.iter().any(|a| a == "--self-test") {
        let stats = store.stats().expect("stats");
        println!(
            "persistwise-desktop {} ok: data_dir={} pages={}",
            env!("CARGO_PKG_VERSION"),
            root.display(),
            stats.pages
        );
        return;
    }

    let smoke = args.iter().any(|a| a == "--smoke-test");
    // `--background` (Windows sign-in start): run hidden in the tray, only
    // when the user has "keep checking feeds when closed" on.
    let background_launch = args.iter().any(|a| a == "--background");
    if background_launch && !(cfg!(windows) && settings::load(&root).feeds.background) {
        return;
    }
    background::sync_windows(&root);
    let protocol_store = store.clone();
    let poller_store = store.clone();
    let builder = tauri::Builder::default();
    // Windows: a second launch shows the running window instead of starting
    // another copy (it may be hidden in the tray).
    #[cfg(windows)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        show_main(app);
    }));
    builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState { store })
        .register_uri_scheme_protocol("pwcopy", move |_ctx, request| {
            serve_saved_copy(&protocol_store, &request)
        })
        .setup(move |app| {
            let poller = poller::Poller::start(app.handle().clone(), poller_store.clone());
            app.manage(poller);
            #[cfg(windows)]
            tray::install(app)?;
            if !background_launch {
                show_main(app.handle());
            }
            if smoke {
                // Exercises the real webview: IPC, the app CSP and the saved
                // copy protocol. The page reports back through smoke_report.
                if let Some(w) = app.get_webview_window("main") {
                    let w2 = w.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        let _ = w2.eval(SMOKE_JS);
                    });
                }
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(120));
                    eprintln!("smoke test: timed out");
                    handle.exit(4);
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Windows with background checking on: closing hides to the tray.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if cfg!(windows) {
                    let root = window.state::<AppState>().store.root().to_path_buf();
                    if settings::load(&root).feeds.background {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            list_pages,
            get_page,
            list_feeds,
            save_page,
            send_to_wayback,
            pin_to_ipfs,
            delete_page,
            get_settings,
            save_settings,
            test_ipfs,
            import_export,
            open_data_dir,
            open_saved_file,
            open_external,
            ipfs_link,
            omarchy_theme,
            smoke_report,
            add_feed,
            localize_page,
            check_feeds,
            polling_now,
            set_feed_paused,
            remove_feed,
            background_status,
            set_background
        ])
        .run(tauri::generate_context!())
        .expect("error while running Persistwise");
}
