# Persistwise Desktop — STATUS

STATUS: PR open, ready for Tim to review (not merged, not tagged, nothing released or submitted)

- Repo (private): https://github.com/ninepointlabs/persistwise-desktop
- PR (open, unmerged): https://github.com/ninepointlabs/persistwise-desktop/pull/1 (feat/v0.1 → main)
- `main` holds only a stub README and the MIT LICENSE. All work is on `feat/v0.1`.
- Last updated: 2026-11-23

## What v0.1 does

| Brief item | State |
| --- | --- |
| 1. Save a page locally (full copy) | Done. HTML plus stylesheets (with `@import`), images (incl. lazy-loaded `data-src`), icons and fonts inlined as `data:` URIs into one file; scripts and inline handlers removed; non-HTML (PDF etc.) kept byte for byte. |
| 2. Browse and search offline | Done. SQLite + FTS5 over title, URL, page text and feed name, ranked, with highlighted snippets. Saved copies open in a sandboxed viewer (no scripts, no forms, CSP `sandbox`). |
| 3. Save Page Now | Done, **on by default** (Tim, 2026-11-23). Public SPN only, no keys. Records the snapshot URL; failure never touches the local copy; retry button. If SPN declines because the URL hit its daily capture limit, links the newest existing snapshot via the public availability API. |
| 4. IPFS settings page | Done. Off by default. "My own node" (IPFS Desktop / Kubo RPC, or a hosted Kubo-compatible RPC with the user's token) or "My own Pinata key". Test connection, pin-on-save, gateway, links to setup docs. Never reads the web app's `PINATA_JWT`. |
| 5. Import from the web account | Done as a **one-time migration** (Tim: "just a tool to get set up one time"): read-only script `tools/web-export/export_archive.rb`, run once with `bin/rails runner` against the web app, writes the export zip; desktop imports it (UI or `--import`). Tested end to end against a throwaway copy of the web code with fake data. No web code changed. Steps: `docs/web-migration.md`. |
| Feed polling (added on Tim's direction) | Done. Follow RSS/Atom/JSON feeds (or a site that links its feed); every new post is saved, sent to Wayback, pinned if IPFS is set up. Checked every 15 min while open; Linux opt-in systemd **user** timer runs `--poll` when the app is closed. Failed downloads retried, backups retried ~hourly, lock file so two pollers never overlap. Imported feeds keep being checked. |
| 6. Packaging | AppImage: built and checked locally (see below). Flatpak: manifest + script + CI job written, **not built locally** (no flatpak here). MSIX: manifest, Store assets, script + CI job written, **not built** (no Windows here). |

## Stack and why

**Tauri 2 (Rust core + the system webview) with a plain HTML/JS UI.**

- One codebase for Linux and Windows. The archive engine (`crates/core`) is plain Rust with no GUI code, so it is tested without a display.
- Small: Windows exe is ~9 MB (uses the WebView2 that ships with Windows 11). The Linux AppImage is 104 MB because it bundles WebKitGTK so it runs on any distro (the Flatpak uses the GNOME runtime's WebKitGTK instead).
- Store-friendly: a single Win32 exe drops straight into MSIX (full-trust desktop app). AppImage and Flatpak bundle are standard outputs.
- No Electron-sized Chromium, no Python runtime to ship, no server.
- UI has no npm dependencies and no build step (`ui/`); npm is only used for the pinned Tauri CLI.
- No telemetry anywhere. The only network traffic is fetching pages the user saves, plus SPN/IPFS when the user turns them on.

Data: `~/.local/share/persistwise` (Linux), `%APPDATA%\Nine Point Labs\Persistwise\data` (Windows), `~/.var/app/com.ninepointlabs.Persistwise/data/persistwise` (Flatpak); `PERSISTWISE_DATA_DIR` overrides. One SQLite file plus `pages/<ab>/<uid>.<ext>`.

## How to run and build

```sh
npm ci
npx tauri dev                       # run
cargo test --workspace              # tests, no network needed
./scripts/build-appimage.sh         # dist/persistwise-desktop-0.1.0-x86_64.AppImage (+ .zsync, .sha256)
./scripts/build-flatpak.sh          # dist/…flatpak (needs flatpak; uses Flathub's org.flatpak.Builder)
pwsh scripts/build-msix.ps1         # Windows: dist\…-x64.msix (needs Windows SDK)
```

CI (`.github/workflows/`): `linux.yml` (tests, AppImage on ubuntu-22.04, AppImage checks under xvfb, Flatpak bundle, uploads only AppImage/zsync/sha256/flatpak to a release, additive `--clobber`), `windows.yml` (tests, exe, MSIX as artifacts only). Both run on every pull request (Tim, 2026-11-23); Linux also on release.

## Tests actually run (2026-11-23, on TUF-Omarchy)

- `cargo test --workspace`: **33 passed, 0 failed** (core unit 14, capture/backup integration 11, import integration 6, desktop 2). All network tests use a local test server; no internet needed.
  - capture: self-contained output, scripts/handlers/refresh/preload removed, stylesheet + `@import` + font + backgrounds + lazy image + icon inlined, `&amp;` in URLs, picture/srcset, missing asset stays an absolute link, no duplicate downloads, PDF kept raw, 404 and bad URLs save nothing.
  - Wayback: redirect answer, job polling answer, 429 recorded as failure with local copy intact, "excluded" message surfaced, daily-limit → latest snapshot.
  - IPFS: Kubo pin records CID and uploads the saved file; node down → failed status, page intact; IPFS off → clean no-op; Pinata uses only the user's own JWT.
  - Import: folder and zip (incl. top-level folder), re-import is idempotent, newer export updates title/Wayback/copy, checksum mismatch skipped and reported, missing copy counted, zip-slip and `../` manifest paths refused, non-exports rejected.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean. `cargo fmt --check`: clean.
- Live (real internet, from this machine):
  - Saved example.com, rust-lang.org (107 resources inlined, 2.3 MB), en.wikipedia.org/wiki/Internet_Archive (163 inlined). Opened the rust-lang and Wikipedia copies in headless Chromium **with all network blocked**: both render fully styled with images.
  - Save Page Now: a fresh capture of a doc.rust-lang.org page succeeded in ~19 s and recorded the snapshot URL; example.com hit SPN's "already captured 5 times today" limit and the app linked the newest snapshot instead.
- Real app window (debug and the AppImage), via `--smoke-test` on Hyprland: IPC works, UI renders the page list, the saved-copy protocol returns 200 with the `sandbox` CSP, unknown id returns 404.
- UI screenshots (headless Chromium with a mocked backend, throwaway profile in the session scratchpad): page list, reader, settings, import, empty state, light/dark, and the Omarchy palette. Found and fixed two UI bugs this way.
- Windows: `cargo zigbuild --release --target x86_64-pc-windows-gnu` builds the exe (8.9 MB). **Not run on Windows.**

### AppImage checklist (standard §6), local build

- [x] `desktop-file-validate` repo copy and AppDir copy: clean.
- [x] `appstreamcli validate --no-net`: passes (pedantic: uppercase letters in the id, expected).
- [x] Icons 16–512 at the right pixel sizes, scalable SVG, root icon, `.DirIcon`.
- [ ] appimagelint: **not run locally** (I did not run an unpinned downloaded binary). It runs report-only in `linux.yml`. objdump on the local build shows GLIBC_2.44 because this machine is Arch; release builds must come from the ubuntu-22.04 CI job (glibc 2.35).
- [x] Headless start: `--self-test` via FUSE and via `--appimage-extract-and-run`. (No xvfb here; real Wayland smoke test instead. CI has the xvfb run.)
- [x] Window class: Wayland app_id `com.ninepointlabs.Persistwise` = desktop id. X11 (XWayland) `WM_CLASS = "com.ninepointlabs.Persistwise", "Com.ninepointlabs.Persistwise"`: instance matches `StartupWMClass`; GTK capitalises the class part.
- [x] Degrade: ran the AppImage with an empty temp `HOME`, `PATH=/usr/bin:/bin` (no hyprctl) and a minimal env: started, smoke test passed, default palette.
- [x] Child env: the app process has the bundled `LD_LIBRARY_PATH`/`GTK_PATH`; `--print-child-env` shows children get the host values back (none of the bundled paths).
- [x] `--install-desktop-entry` / `--remove-desktop-entry` in a temp `XDG_DATA_HOME`: desktop file + 9 icons created, `Exec` = quoted `$APPIMAGE`, validates; removal leaves no files. Run from an extracted dir it explains and does nothing.
- [ ] Omarchy dock/Walker grouping with the icon: app_id matches; I did not install the entry into Tim's real menu.
- [ ] Release dry run in CI: not run (the attach job only runs on a published release or a tagged manual run).
- Size: 104 MB (WebKitGTK + GTK bundled).

## Packaging status and exact remaining steps

**AppImage**: done locally. Remaining: run `linux.yml` once (Actions → Linux → Run workflow) to get the glibc-2.35 build and the CI checks. Tauri's bundler downloads linuxdeploy from tauri-apps' own pinned release URLs plus `linuxdeploy-plugin-appimage` from a `continuous` URL (not hash-pinned by Tauri); appimagetool 1.9.1 is pinned by SHA-256 in `scripts/build-appimage.sh`.

**Flatpak bundle (self-hosted, never Flathub)**: `packaging/flatpak/com.ninepointlabs.Persistwise.yml` (GNOME 50 runtime, network + Wayland/X11 + DRI, no home access) and `scripts/build-flatpak.sh`. Remaining: run `linux.yml`; then install the bundle on a test machine and check the file-chooser portal (import) and that "Open original" opens the browser.

**MSIX / Microsoft Store**: `packaging/windows/msix/AppxManifest.xml` (full-trust Win32, `internetClient`, Windows 10 1809+), Store logo assets, `scripts/build-msix.ps1`, `windows.yml`. Remaining, in order:
1. Tim: a Partner Center developer account (company accounts may have a registration fee depending on account type; that is Tim's call, I created nothing).
2. Reserve the name "Persistwise" in Partner Center; copy Package/Identity/Name, Publisher (CN=…) and publisher display name into the `build-msix.ps1` parameters (or CI secrets/vars).
3. Run `windows.yml`, side-load-test the MSIX on Windows 10 and 11 (sign with a local test cert via `-CertPath`), check WebView2 starts and the data folder works under MSIX AppData virtualization.
4. Submit the unsigned MSIX in Partner Center (the Store signs it). Store listing needs screenshots, privacy policy URL (the app collects nothing; a short page still needs to exist), age rating questionnaire. `runFullTrust` is a restricted capability but standard for packaged desktop apps; expect to justify it in one line.

**Windows paid, Linux free (nothing set up)**: simplest path is a paid Store listing. The Store handles payment, licence and refunds; the app needs no licence code at all, and the GitHub release keeps shipping only Linux files. Options if you want more later: a free trial via the Store's trial mode (needs a small `Windows.Services.Store` licence check in the app), or selling outside the Store with signed licence keys (needs a code-signing cert for a non-Store installer and a payment page, which conflicts with "no hosted backend" unless done through a third-party store). No Stripe product, Store listing or price was created.

## Decisions from Tim (2026-11-23)

- Wayback: on by default. Done.
- License: MIT. Kept.
- Web export: a one-time migration tool, not a feature. Replaced the "Download my archive" proposal with a read-only script that runs once (`tools/web-export/export_archive.rb`, `docs/web-migration.md`).
- Feed polling: the desktop app does it, with a background option. Done (in-app timer + Linux systemd user timer; Windows/Flatpak check while open for now).

More decisions from Tim (later on 2026-11-23, "yes, yes, yes, yes"):

- Windows background checking via a startup task: done (see below), untested on Windows.
- First check of a new feed saves everything the feed lists: kept as is.
- App id `com.ninepointlabs.Persistwise`, binary `persistwise-desktop`, name "Persistwise": kept.
- Imported copies get their images embedded: done. Each scheduled/"check all" run (and `--poll`) embeds up to 25 imported pages; "Embed images now" does one page. Original saved date kept; if nothing can be fetched (offline) the page is left alone and retried; re-imports never overwrite an embedded copy.

### Windows background checking (compiles; not run on Windows)

- Setting "Keep checking feeds when Persistwise is closed" (`feeds.background`, on by default on Windows).
- On: closing the window hides it to a tray icon (Open / Check feeds now / Quit); a second launch shows the running window (single-instance plugin).
- Sign-in start with `--background` (window stays hidden): MSIX declares a `windows.startupTask` with `uap10:Parameters="--background"` (Enabled after first launch; users can turn it off in Settings > Apps > Startup). The plain .exe writes/removes `HKCU\...\Run\Persistwise`. A `--background` start with the setting off exits immediately, so the in-app switch works without the WinRT StartupTask API.
- Verified only by cross-compiling (`cargo zigbuild --target x86_64-pc-windows-gnu`) and XML-validating the manifest. Needs a real Windows 10/11 test of: tray icon, close-to-tray, second launch, Run key, and the MSIX startup task actually passing `--background`.

## Import findings (web app read only, `master` @ 703caff)

- Model: `users` → `feeds` (url, name, kind, status, last_polled_at, last_error) → `posts` (guid, url, title, author, published_at, fetched_at, status, local_path) → `archive_records` (original_url, status, wayback_url, wayback_job_id, wayback_submitted_at/failed_at, cid, pin_service, archived_at, pin_failed_at, last_error).
- HTML lives at `storage/archives/<feed_id>/<post_id>.html` with relative URLs made absolute (`SavedPageHtml`); images are not stored. Owner-only `GET /archive/:id/saved-copy`.
- Confirmed: no export button, no API, no JSON endpoints in `config/routes.rb`; the FAQ says "Not yet".
- One-time export script (no web code changed): `tools/web-export/export_archive.rb`, pure Ruby stdlib, run with `bin/rails runner`. Exports one account's feeds, posts, archive records and saved copies (with SHA-256) as a zip. Leaves out passwords, 2FA, sessions, Stripe data and `feeds.last_error`. Tested under the web app's code (Ruby 4.0.6) with a fresh dev DB and fake data in a throwaway copy that was deleted afterwards; zip passes `unzip -t`; only the chosen user's data; imported twice with no duplicates. **Not run against production**; the Railway steps in `docs/web-migration.md` are suggestions for Tim and are untested.
- Imported copies keep remote image links (that is what the web app stored); the viewer allows https images/styles/fonts for that reason.

## Open questions for Tim

None right now.

## Not done / could not do

- No Flatpak or MSIX build was produced (no flatpak or Windows on this machine). Both are scripted and in CI, untested.
- appimagelint not run locally; CI runs it report-only.
- No release, tag, store submission, Partner Center or Stripe work (by design).
- Could not screenshot the real app window (`grim` hangs from this session), so real-window checks are the in-webview `--smoke-test` plus mocked-UI screenshots.
- I did not change, branch, deploy or read secrets from the web Persistwise repo. I read its README, `db/schema.rb`, `config/routes.rb`, the models, the archive jobs, `SavedCopiesController` and the FAQ text. I did not read `PLAN.md` closely (the README covered what I needed), and I did not touch `storage/archives/` or any `.env`.

## PR

https://github.com/ninepointlabs/persistwise-desktop/pull/1 — open, not merged. Merging, tagging, releasing and any store submission each wait for Tim.
