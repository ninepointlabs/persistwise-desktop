// Persistwise Desktop UI. Plain JS, no build step, no third-party code.
"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel) => document.querySelector(sel);
const PAGE_SIZE = 100;
const HIT_START = "";
const HIT_END = "";

const state = {
  query: "",
  feedId: null,
  pages: [],
  offset: 0,
  current: null,
  // The thread the reader is showing, when the current page is part of one.
  thread: null,
  platform: "linux",
  settings: null,
};

// ---------- small helpers ----------

function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === "class") el.className = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

// Search snippets mark hits with private-use characters; escape first, then mark.
function snippetNode(s) {
  const span = document.createElement("p");
  span.className = "snippet";
  span.innerHTML = escapeHtml(s).split(HIT_START).join("<mark>").split(HIT_END).join("</mark>");
  return span;
}

function hostOf(url) {
  try { return new URL(url).host; } catch { return url; }
}

function fmtDate(s) {
  if (!s) return "";
  const d = new Date(s);
  return isNaN(d) ? s : d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

function fmtBytes(n) {
  if (n == null) return "";
  const u = ["B", "KB", "MB", "GB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return `${n.toFixed(i ? 1 : 0)} ${u[i]}`;
}

function setStatus(msg, isError = false) {
  const el = $("#status");
  el.textContent = msg || "";
  el.classList.toggle("error", !!isError);
}

function openExternal(url) {
  invoke("open_external", { url }).catch((e) => setStatus(String(e), true));
}

const BADGE_TEXT = {
  wayback: { done: "Wayback ✓", pending: "Wayback…", failed: "Wayback failed" },
  ipfs: { done: "IPFS ✓", pending: "IPFS…", failed: "IPFS failed" },
};

function badges(p) {
  const out = [p.local_path ? h("span", { class: "badge done" }, "Saved locally")
    : p.capture_error ? h("span", { class: "badge failed", title: p.capture_error }, "Download failed, will retry")
    : h("span", { class: "badge" }, "No local copy")];
  if (p.wayback_status !== "off") out.push(h("span", { class: `badge ${p.wayback_status}` }, BADGE_TEXT.wayback[p.wayback_status] || p.wayback_status));
  if (p.ipfs_status !== "off") out.push(h("span", { class: `badge ${p.ipfs_status}` }, BADGE_TEXT.ipfs[p.ipfs_status] || p.ipfs_status));
  if (p.origin === "import") out.push(h("span", { class: "badge" }, "Imported"));
  if (p.thread_status === "partial") out.push(h("span", { class: "badge pending", title: "Not every page of this thread could be saved" }, "Partial thread"));
  return h("div", { class: "badges" }, out);
}

// ---------- views ----------

function show(view) {
  for (const v of document.querySelectorAll(".view")) v.hidden = v.id !== `view-${view}`;
  for (const b of document.querySelectorAll(".nav-item")) {
    const isPages = b.dataset.view === "pages";
    const active = view === "reader" ? isPages && String(state.feedId ?? "") === b.dataset.feed
      : b.dataset.view === view && (!isPages || String(state.feedId ?? "") === b.dataset.feed);
    b.classList.toggle("active", active);
  }
  if (view === "settings") loadSettings();
  if (view === "about") loadAbout();
}

async function refreshSidebar() {
  const [info, feeds] = await Promise.all([invoke("app_info"), invoke("list_feeds")]);
  state.platform = info.platform;
  $("#count-all").textContent = info.stats.pages || "";
  const box = $("#feeds");
  box.replaceChildren();
  state.feeds = feeds;
  if (!feeds.length) box.append(h("p", { class: "hint", style: "padding:0 8px" }, "Follow a blog or newsletter with +."));
  for (const f of feeds) {
    const flag = f.status === "paused" ? h("span", { class: "paused" }, "paused")
      : f.status === "errored" ? h("span", { class: "errored", title: f.last_error || "" }, "error") : null;
    const b = h("button", { class: "nav-item", "data-view": "pages", "data-feed": String(f.id), title: f.url },
      h("span", { class: "name" }, f.name || hostOf(f.url), " ", flag), h("span", { class: "count" }, f.page_count));
    b.addEventListener("click", () => selectFeed(f.id));
    box.append(b);
  }
  renderFeedHeader();
  return info;
}

function selectFeed(id) {
  state.feedId = id;
  renderFeedHeader();
  loadPages();
  show("pages");
}

function renderFeedHeader() {
  const el = $("#feed-header");
  const f = (state.feeds || []).find((x) => x.id === state.feedId);
  if (!f) { el.hidden = true; el.replaceChildren(); return; }
  el.hidden = false;
  const paused = f.status === "paused";
  el.replaceChildren(
    h("h2", {}, f.name || hostOf(f.url)),
    h("div", { class: "meta" },
      h("span", {}, f.url),
      h("span", {}, f.last_polled_at ? `Checked ${fmtDateTime(f.last_polled_at)}` : "Not checked yet"),
      paused ? h("span", {}, "Paused") : null,
      f.origin === "import" ? h("span", {}, "Imported from Persistwise web") : null,
      f.last_error ? h("span", { class: "err" }, `Last check failed: ${f.last_error}`) : null),
    h("div", { class: "row" },
      h("button", { onclick: () => checkFeeds(f.id) }, "Check now"),
      h("button", { onclick: () => pauseFeed(f, !paused) }, paused ? "Resume" : "Pause"),
      h("button", { class: "danger", onclick: () => removeFeed(f) }, "Stop following")),
  );
}

function fmtDateTime(s) {
  const d = new Date(s);
  return isNaN(d) ? s : d.toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

async function checkFeeds(id = null) {
  await invoke("check_feeds", { feedId: id });
  $("#poll-status").textContent = "Checking feeds…";
}

async function pauseFeed(f, paused) {
  await invoke("set_feed_paused", { id: f.id, paused });
  await refreshSidebar();
}

async function removeFeed(f) {
  if (!confirm(`Stop following “${f.name || f.url}”? Pages already saved from it stay in your archive.`)) return;
  await invoke("remove_feed", { id: f.id });
  state.feedId = null;
  await refreshSidebar();
  await loadPages();
  show("pages");
}

$("#feed-add-toggle").addEventListener("click", () => {
  const form = $("#feed-add");
  form.hidden = !form.hidden;
  if (!form.hidden) $("#feed-url").focus();
});
$("#feeds-check").addEventListener("click", () => checkFeeds(null));
$("#feed-add").addEventListener("submit", async (e) => {
  e.preventDefault();
  let url = $("#feed-url").value.trim();
  if (!url) return;
  if (!/^https?:\/\//i.test(url)) url = `https://${url}`;
  const btn = $("#feed-add-btn");
  btn.disabled = true;
  btn.textContent = "Looking…";
  try {
    const feed = await invoke("add_feed", { url });
    $("#feed-url").value = "";
    $("#feed-add").hidden = true;
    $("#poll-status").textContent = `Following ${feed.name || hostOf(feed.url)}. Saving its posts…`;
    await refreshSidebar();
    selectFeed(feed.id);
  } catch (err) {
    alert(`Could not follow that: ${err}`);
  } finally {
    btn.disabled = false;
    btn.textContent = "Follow";
  }
});

listen("poll-state", async (e) => {
  const st = e.payload;
  const el = $("#poll-status");
  if (st.running) { el.textContent = "Checking feeds…"; return; }
  if (st.message) { el.textContent = st.message; }
  else {
    const n = st.reports.reduce((a, r) => a + r.new_pages, 0);
    const errs = st.reports.filter((r) => r.error).length;
    el.textContent = `Checked ${st.reports.length} feed${st.reports.length === 1 ? "" : "s"} at ${new Date().toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}: ${n} new post${n === 1 ? "" : "s"}${errs ? `, ${errs} failed` : ""}.`;
  }
  await refreshSidebar();
  if (!$("#view-pages").hidden) await loadPages();
});

async function loadPages(append = false) {
  if (!append) state.offset = 0;
  try {
    const rows = await invoke("list_pages", { query: state.query || null, feedId: state.feedId, limit: PAGE_SIZE, offset: state.offset });
    state.pages = append ? state.pages.concat(rows) : rows;
    state.offset += rows.length;
    renderPages(rows.length === PAGE_SIZE);
  } catch (e) {
    setStatus(String(e), true);
  }
}

function renderPages(hasMore) {
  const list = $("#page-list");
  list.replaceChildren(...state.pages.map(pageItem));
  $("#empty").hidden = state.pages.length > 0 || !!state.query || state.feedId != null;
  $("#more").hidden = !hasMore;
  if (!state.pages.length && state.query) setStatus(`Nothing in your archive matches “${state.query}”.`);
}

function pageItem(p) {
  // Without a search, a thread shows as one item (its first page stands in
  // for it); a search hit inside a thread says which page it is on.
  const asThread = p.thread_id != null && !p.snippet && !state.query;
  const threadTitle = p.thread_title || p.title || hostOf(p.url);
  const li = h("li", { class: "page", tabindex: "0", "data-id": String(p.id) },
    h("h4", {}, asThread ? threadTitle : (p.title || hostOf(p.url))),
    h("div", { class: "meta" },
      h("span", {}, hostOf(p.url)),
      p.feed_name ? h("span", {}, p.feed_name) : null,
      asThread ? h("span", {}, `${p.thread_page_count} page${p.thread_page_count === 1 ? "" : "s"}`) : null,
      !asThread && p.thread_id != null ? h("span", {}, `Page ${p.thread_page} of ${p.thread_page_count} in “${threadTitle}”`) : null,
      h("span", {}, asThread ? `Saved ${fmtDate(p.thread_saved_at || p.saved_at)}`
        : p.published_at ? `Published ${fmtDate(p.published_at)}` : `Saved ${fmtDate(p.saved_at)}`),
      !asThread && p.size_bytes ? h("span", {}, fmtBytes(p.size_bytes)) : null),
    p.snippet ? snippetNode(p.snippet) : null,
    asThread ? h("div", { class: "badges" }, h("span", { class: "badge thread" }, "Thread"),
      p.thread_status === "partial" ? h("span", { class: "badge pending", title: "Not every page of this thread could be saved" }, "Partial thread") : null)
      : badges(p));
  li.addEventListener("click", () => openReader(p.id));
  li.addEventListener("keydown", (e) => { if (e.key === "Enter") openReader(p.id); });
  return li;
}

function copyUrl(id) {
  return state.platform === "windows" ? `http://pwcopy.localhost/${id}` : `pwcopy://localhost/${id}`;
}

async function openReader(id) {
  const p = await invoke("get_page", { id });
  if (!p) return;
  state.current = p;
  state.thread = p.thread_id != null ? await invoke("get_thread", { id: p.thread_id }) : null;
  renderReader(p, true);
  show("reader");
  $("#main").scrollTop = 0;
}

function renderThreadNav(p) {
  const nav = $("#thread-nav");
  const t = state.thread;
  if (!t || p.thread_id !== t.id) { nav.hidden = true; nav.replaceChildren(); return; }
  const idx = t.pages.findIndex((x) => x.id === p.id);
  const prev = t.pages[idx - 1];
  const next = t.pages[idx + 1];
  const select = h("select", { "aria-label": "Page of this thread" },
    t.pages.map((x, i) => h("option", { value: String(x.id), selected: x.id === p.id }, `Page ${i + 1} of ${t.pages.length}`)));
  select.addEventListener("change", () => openReader(Number(select.value)));
  nav.hidden = false;
  // replaceChildren would print a null child as the text "null".
  nav.replaceChildren(...[
    h("strong", {}, "Thread"),
    h("button", { disabled: !prev, onclick: () => prev && openReader(prev.id) }, "‹ Previous page"),
    select,
    h("button", { disabled: !next, onclick: () => next && openReader(next.id) }, "Next page ›"),
    t.status === "partial" ? h("span", { class: "note", title: t.note || "" }, `Partial thread${t.note ? `: ${t.note}` : ""}`) : null,
    h("span", { class: "spacer" }),
    h("button", { class: "danger", onclick: () => deleteThread(t) }, "Delete thread"),
  ].filter(Boolean));
}

async function renderReader(p, reloadFrame) {
  $("#reader-title").textContent = p.title || hostOf(p.url);
  $("#reader-url").textContent = p.url;
  const actions = $("#reader-actions");
  actions.replaceChildren(...[
    h("button", { onclick: () => openExternal(p.url) }, "Open original"),
    p.wayback_url ? h("button", { onclick: () => openExternal(p.wayback_url) }, "Open in Wayback") : null,
    p.wayback_status !== "pending" ? h("button", { onclick: () => sendWayback(p.id) }, p.wayback_url ? "Capture again" : "Send to Wayback") : null,
    p.cid ? h("button", { onclick: async () => openExternal(await invoke("ipfs_link", { cid: p.cid })) }, "Open on IPFS") : null,
    !p.cid && p.ipfs_status !== "pending" && p.local_path ? h("button", { onclick: () => pinIpfs(p.id) }, "Pin to IPFS") : null,
    p.origin === "import" && p.local_path && !p.localized_at && p.content_type === "text/html"
      ? h("button", { onclick: (e) => embedImages(p.id, e.target) }, "Embed images now") : null,
    p.local_path ? h("button", { onclick: () => invoke("open_saved_file", { id: p.id }).catch((e) => alert(e)) }, "Open file") : null,
    h("button", { class: "danger", onclick: () => deletePage(p) }, p.thread_id != null ? "Delete this page" : "Delete"),
  ].filter(Boolean));
  renderThreadNav(p);
  const meta = $("#reader-meta");
  meta.replaceChildren(...[
    h("span", {}, `Saved ${fmtDate(p.saved_at)}`),
    p.size_bytes ? h("span", {}, fmtBytes(p.size_bytes)) : null,
    p.origin !== "import" ? h("span", {}, `${p.assets_inlined} resources inlined${p.assets_failed ? `, ${p.assets_failed} could not be` : ""}`) : h("span", {}, p.localized_at ? `Imported from Persistwise web; ${p.assets_inlined} images and styles embedded`
      : "Imported from Persistwise web; images still load from the original site until they are embedded (on the next feed check)"),
    p.capture_error && !p.local_path ? h("span", { class: "err" }, `Download failed (will retry on the next check): ${p.capture_error}`) : null,
    p.wayback_status === "pending" ? h("span", {}, "Waiting for Save Page Now…") : null,
    p.wayback_error && p.wayback_status === "failed" ? h("span", { class: "err" }, `Wayback: ${p.wayback_error}`) : null,
    p.ipfs_status === "pending" ? h("span", {}, "Pinning to IPFS…") : null,
    p.cid ? h("span", {}, `CID ${p.cid}`) : null,
    p.ipfs_error && p.ipfs_status === "failed" ? h("span", { class: "err" }, `IPFS: ${p.ipfs_error}`) : null,
    h("span", {}, "Links in saved copies are inactive; use Open original."),
  ].filter(Boolean));
  if (reloadFrame) {
    const frame = $("#reader-frame");
    if (!p.local_path) {
      frame.removeAttribute("src");
      frame.srcdoc = "<p style='font:14px system-ui;padding:20px'>There is no local copy of this page.</p>";
    } else if (p.content_type === "text/html" || p.content_type.startsWith("image/") || p.content_type === "text/plain") {
      frame.removeAttribute("srcdoc");
      frame.src = copyUrl(p.id);
    } else {
      frame.removeAttribute("src");
      frame.srcdoc = "<p style='font:14px system-ui;padding:20px'>This saved file is not a web page. Use “Open file” to view it.</p>";
    }
  }
}

async function embedImages(id, btn) {
  btn.disabled = true;
  btn.textContent = "Embedding…";
  try {
    const p = await invoke("localize_page", { id });
    state.current = p;
    renderReader(p, true);
  } catch (e) {
    alert(`Could not embed images: ${e}`);
    btn.disabled = false;
    btn.textContent = "Embed images now";
  }
}

async function sendWayback(id) {
  try { await invoke("send_to_wayback", { id }); await refreshCurrent(id); } catch (e) { alert(e); }
}

async function pinIpfs(id) {
  try { await invoke("pin_to_ipfs", { id }); await refreshCurrent(id); } catch (e) { alert(e); }
}

async function refreshCurrent(id) {
  const p = await invoke("get_page", { id });
  if (p && state.current && state.current.id === id) { state.current = p; renderReader(p, false); }
  const idx = state.pages.findIndex((x) => x.id === id);
  if (p && idx >= 0) { state.pages[idx] = { ...p, snippet: state.pages[idx].snippet }; renderPages(!$("#more").hidden); }
}

async function deleteThread(t) {
  if (!confirm(`Delete all ${t.pages.length} saved pages of the thread “${t.title || t.url}” from this computer? This cannot be undone. (Wayback and IPFS copies are not affected.)`)) return;
  await invoke("delete_thread", { id: t.id });
  state.current = null;
  state.thread = null;
  await refreshSidebar();
  await loadPages();
  show("pages");
}

async function deletePage(p) {
  if (!confirm(`Delete the saved copy of “${p.title || p.url}” from this computer? This cannot be undone. (Wayback and IPFS copies are not affected.)`)) return;
  await invoke("delete_page", { id: p.id });
  state.current = null;
  await refreshSidebar();
  await loadPages();
  show("pages");
}

// ---------- saving ----------

$("#save-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $("#save-url");
  let url = input.value.trim();
  if (!url) return;
  if (!/^https?:\/\//i.test(url)) url = `https://${url}`;
  const btn = $("#save-btn");
  const wholeThread = $("#save-thread").checked;
  btn.disabled = true;
  btn.textContent = "Saving…";
  setStatus(wholeThread ? `Saving page 1 of … (${url})` : `Saving ${url} …`);
  try {
    if (wholeThread) {
      const r = await invoke("save_thread", { url, wayback: $("#save-wayback").checked });
      input.value = "";
      $("#save-thread").checked = false;
      const title = r.thread?.title || r.pages[0]?.title || url;
      setStatus(`“${title}”: ${r.message}`);
    } else {
      const page = await invoke("save_page", { url, wayback: $("#save-wayback").checked });
      input.value = "";
      setStatus(`Saved “${page.title || url}”${page.assets_failed ? ` (${page.assets_failed} resources could not be fetched)` : ""}.`);
    }
    state.query = "";
    $("#search").value = "";
    state.feedId = null;
    await refreshSidebar();
    await loadPages();
    show("pages");
  } catch (err) {
    setStatus(`Could not save: ${err}`, true);
  } finally {
    btn.disabled = false;
    btn.textContent = "Save";
  }
});

listen("thread-progress", (e) => {
  const p = e.payload;
  setStatus(`Saving page ${p.page} of … (${p.url})`);
  $("#save-btn").textContent = `Page ${p.page}…`;
});

// ---------- search & nav ----------

let searchTimer;
$("#search").addEventListener("input", (e) => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => {
    state.query = e.target.value.trim();
    setStatus("");
    loadPages();
    show("pages");
  }, 180);
});

document.querySelector('.nav-item[data-view="pages"]').addEventListener("click", () => selectFeed(null));
for (const b of document.querySelectorAll('.nav-bottom .nav-item')) b.addEventListener("click", () => show(b.dataset.view));
$("#reader-back").addEventListener("click", () => show("pages"));
$("#more").addEventListener("click", () => loadPages(true));
document.addEventListener("click", (e) => {
  const ext = e.target.closest("[data-ext]");
  if (ext) { e.preventDefault(); openExternal(ext.dataset.ext); }
  const go = e.target.closest("[data-goto]");
  if (go) { e.preventDefault(); show(go.dataset.goto); }
});

// ---------- import ----------

async function runImport(folder) {
  const out = $("#import-result");
  out.replaceChildren(h("p", { class: "hint" }, "Waiting for you to choose…"));
  try {
    const res = await invoke("import_export", { folder });
    if (!res) { out.replaceChildren(); return; }
    const r = res.report;
    out.replaceChildren(
      h("h2", {}, "Import finished"),
      h("p", { class: "hint" }, res.path),
      h("table", { class: "report" },
        h("tr", {}, h("td", {}, "Feeds"), h("td", {}, `${r.feeds_created} new, ${r.feeds_updated} updated`)),
        h("tr", {}, h("td", {}, "Pages"), h("td", {}, `${r.pages_created} new, ${r.pages_updated} updated`)),
        h("tr", {}, h("td", {}, "Saved copies"), h("td", {}, `${r.copies_saved} written, ${r.copies_unchanged} unchanged, ${r.copies_missing} missing from export`))),
      r.problems.length ? h("div", { class: "problems" }, h("p", {}, `${r.problems.length} item(s) were skipped:`), h("ul", {}, r.problems.map((p) => h("li", {}, p)))) : "",
    );
    await refreshSidebar();
    await loadPages();
  } catch (e) {
    out.replaceChildren(h("p", { class: "problems" }, `Import failed: ${e}`));
  }
}
$("#import-zip").addEventListener("click", () => runImport(false));
$("#import-folder").addEventListener("click", () => runImport(true));

// ---------- settings ----------

function ipfsModeChanged() {
  const mode = document.querySelector('input[name="ipfs-mode"]:checked')?.value || "off";
  $("#kubo-fields").hidden = mode !== "kubo";
  $("#pinata-fields").hidden = mode !== "pinata";
  $("#ipfs-common").hidden = mode === "off";
}
for (const r of document.querySelectorAll('input[name="ipfs-mode"]')) r.addEventListener("change", ipfsModeChanged);

function fillSettings(view) {
  state.settings = view.settings;
  const s = view.settings;
  $("#set-wayback").checked = s.wayback_on_save;
  $("#poll-minutes").value = String(s.feeds.poll_minutes);
  const radio = document.querySelector(`input[name="ipfs-mode"][value="${s.ipfs.mode}"]`);
  if (radio) radio.checked = true;
  $("#kubo-url").value = s.ipfs.kubo_api_url;
  $("#pin-on-save").checked = s.ipfs.pin_on_save;
  $("#gateway").value = s.ipfs.gateway;
  $("#gateway").placeholder = view.gateway_default;
  $("#kubo-token").value = "";
  $("#pinata-jwt").value = "";
  $("#kubo-token-state").textContent = view.kubo_token ? `Saved token ${view.kubo_token}. Type a new one to replace it, or a single space to remove it.` : "";
  $("#pinata-jwt-state").textContent = view.pinata_jwt ? `Saved key ${view.pinata_jwt}. Paste a new one to replace it, or a single space to remove it.` : "";
  ipfsModeChanged();
}

async function loadSettings() {
  const [view, bg, loc] = await Promise.all([invoke("get_settings"), invoke("background_status"), invoke("archive_location")]);
  fillSettings(view);
  fillLocation(loc);
  fillBackground(bg);
}

// ---------- archive folder ----------

function fillLocation(loc) {
  $("#data-dir").textContent = loc.path;
  $("#folder-choose").disabled = loc.env_locked;
  $("#folder-default").hidden = loc.is_default || loc.env_locked;
  $("#folder-hint").textContent = loc.env_locked
    ? "The PERSISTWISE_DATA_DIR environment variable sets this folder, so it cannot be changed here."
    : loc.is_default ? "This is the default folder."
    : `This is a folder you chose. The default is ${loc.default_path}.`;
}

function folderChoice(...children) {
  const box = $("#folder-choice");
  box.hidden = children.length === 0;
  box.replaceChildren(...children);
}

async function afterFolderChange(res) {
  folderChoice();
  fillLocation(res.location);
  $("#folder-hint").textContent = `Now using ${res.location.path}.` + (res.warning ? ` Note: ${res.warning}` : "");
  state.current = null;
  state.thread = null;
  state.feedId = null;
  state.query = "";
  $("#search").value = "";
  await refreshSidebar();
  await loadPages();
  const view = await invoke("get_settings");
  fillSettings(view);
  $("#save-wayback").checked = view.settings.wayback_on_save;
  fillBackground(await invoke("background_status"));
}

async function changeFolder(path, how, btns) {
  for (const b of btns) b.disabled = true;
  $("#folder-hint").textContent = how === "move" ? "Moving your archive… (large archives take a while)"
    : how === "copy" ? "Copying your archive… (large archives take a while)" : "Switching…";
  try {
    await afterFolderChange(await invoke("change_archive_folder", { path, how }));
  } catch (e) {
    $("#folder-hint").textContent = `Could not change the folder: ${e}`;
    for (const b of btns) b.disabled = false;
  }
}

function offerFolder(check) {
  const where = check.is_default ? `the default folder (${check.path})` : check.path;
  if (check.kind === "existing") {
    const open = h("button", { class: "primary" }, "Open that archive");
    const cancel = h("button", { onclick: () => folderChoice() }, "Cancel");
    open.addEventListener("click", () => changeFolder(check.path, "open", [open, cancel]));
    folderChoice(
      h("p", {}, h("strong", {}, `${where} already holds a Persistwise archive.`)),
      h("p", { class: "hint" }, "Persistwise will open it as it is. Your current archive stays where it is, untouched; nothing is merged or overwritten."),
      h("div", { class: "row" }, open, cancel));
    return;
  }
  const move = h("button", { class: "primary" }, "Move my archive there (recommended)");
  const copy = h("button", {}, "Copy it there");
  const fresh = h("button", {}, "Start a fresh, empty archive there");
  const cancel = h("button", { class: "ghost", onclick: () => folderChoice() }, "Cancel");
  const all = [move, copy, fresh, cancel];
  move.addEventListener("click", () => changeFolder(check.path, "move", all));
  copy.addEventListener("click", () => changeFolder(check.path, "copy", all));
  fresh.addEventListener("click", () => changeFolder(check.path, "fresh", all));
  folderChoice(
    h("p", {}, h("strong", {}, `Use ${where} for your archive?`)),
    h("p", { class: "hint" }, "Move: everything (pages, feeds, settings and keys) goes to the new folder and is removed from the old one once the copy is checked, so you keep one archive. "
      + "Copy: the old folder keeps a full copy too. Start fresh: an empty archive; your current one stays where it is."),
    h("div", { class: "row" }, move, copy, fresh, cancel));
}

$("#folder-choose").addEventListener("click", async () => {
  folderChoice();
  try {
    const check = await invoke("pick_archive_folder");
    if (check) offerFolder(check);
  } catch (e) {
    $("#folder-hint").textContent = `That folder cannot be used: ${e}`;
  }
});

$("#folder-default").addEventListener("click", async () => {
  folderChoice();
  try { offerFolder(await invoke("check_default_folder")); }
  catch (e) { $("#folder-hint").textContent = `The default folder cannot be used: ${e}`; }
});

function fillBackground(bg) {
  const box = $("#bg-enabled");
  box.checked = bg.enabled;
  box.disabled = !bg.supported;
  const win = state.platform === "windows";
  $("#bg-status").textContent = !bg.supported ? bg.reason
    : win ? (bg.enabled
      ? "On: closing the window keeps Persistwise in the tray (right-click the icon to quit), and it starts hidden when you sign in."
      : "Off: closing the window quits Persistwise, and it does not start when you sign in.")
    : bg.enabled ? `On: a systemd user timer runs ${bg.command}. If you move the AppImage, turn this off and on again.`
    : "Uses a systemd user timer on this computer (no root, nothing runs on our servers).";
}

$("#bg-enabled").addEventListener("change", async (e) => {
  try { fillBackground(await invoke("set_background", { enabled: e.target.checked })); }
  catch (err) { e.target.checked = !e.target.checked; $("#bg-status").textContent = `Could not change it: ${err}`; }
});

function secretField(el) {
  const v = el.value;
  if (v === "") return null; // keep
  return v.trim() === "" ? "" : v.trim(); // a lone space clears it
}

$("#settings-save").addEventListener("click", async () => {
  const s = structuredClone(state.settings);
  s.wayback_on_save = $("#set-wayback").checked;
  s.feeds.poll_minutes = Number($("#poll-minutes").value);
  s.ipfs.mode = document.querySelector('input[name="ipfs-mode"]:checked')?.value || "off";
  s.ipfs.kubo_api_url = $("#kubo-url").value.trim() || "http://127.0.0.1:5001";
  s.ipfs.pin_on_save = $("#pin-on-save").checked;
  s.ipfs.gateway = $("#gateway").value.trim();
  const secrets = {};
  const kt = secretField($("#kubo-token"));
  if (kt !== null) secrets.kubo_token = kt;
  const pj = secretField($("#pinata-jwt"));
  if (pj !== null) secrets.pinata_jwt = pj;
  try {
    const view = await invoke("save_settings", { settings: s, secrets });
    fillSettings(view);
    $("#save-wayback").checked = view.settings.wayback_on_save;
    $("#settings-saved").textContent = "Saved.";
    setTimeout(() => ($("#settings-saved").textContent = ""), 2500);
  } catch (e) {
    $("#settings-saved").textContent = `Could not save: ${e}`;
  }
});

$("#ipfs-test").addEventListener("click", async () => {
  const out = $("#ipfs-test-result");
  out.textContent = "Testing… (save settings first if you changed them)";
  try { out.textContent = await invoke("test_ipfs"); } catch (e) { out.textContent = String(e); }
});

$("#open-data").addEventListener("click", () => invoke("open_data_dir").catch((e) => alert(e)));

async function loadAbout() {
  const info = await invoke("app_info");
  $("#about-version").textContent = `Version ${info.version} · ${info.stats.pages} pages · data in ${info.data_dir}`;
}

// ---------- Omarchy theme follow (falls back to light/dark system palette) ----------

const THEME_MAP = {
  "--bg": "background", "--bg2": "dark_background", "--bg3": "lighter_background",
  "--fg": "foreground", "--muted": "dark_foreground", "--accent": "accent",
  "--sel": "selection", "--ok": "green", "--warn": "yellow", "--bad": "red",
};
let themeGen = null;
async function applyTheme() {
  let t = null;
  try { t = await invoke("omarchy_theme"); } catch { /* not on Omarchy */ }
  if (!t) {
    if (themeGen !== null) { for (const k of Object.keys(THEME_MAP)) document.documentElement.style.removeProperty(k); themeGen = null; }
    return false;
  }
  if (t.generation === themeGen) return true;
  themeGen = t.generation;
  const root = document.documentElement.style;
  for (const [cssVar, key] of Object.entries(THEME_MAP)) {
    if (t.colors[key]) root.setProperty(cssVar, t.colors[key]);
  }
  root.setProperty("--border", t.colors.selection || t.colors.lighter_background || "");
  root.setProperty("--accent-fg", t.colors.background || "#000");
  root.setProperty("color-scheme", t.mode === "light" ? "light" : "dark");
  return true;
}

// ---------- start ----------

listen("page-updated", (e) => refreshCurrent(e.payload.id));

(async function start() {
  try {
    const info = await refreshSidebar();
    const view = await invoke("get_settings");
    state.settings = view.settings;
    $("#save-wayback").checked = view.settings.wayback_on_save;
    await loadPages();
    if (info.omarchy && (await applyTheme())) setInterval(applyTheme, 4000);
  } catch (e) {
    setStatus(`Could not open your archive: ${e}`, true);
  }
})();
