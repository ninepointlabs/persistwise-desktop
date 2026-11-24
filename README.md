# Persistwise Desktop

A personal web archive that runs entirely on your own computer.

Paste a link, and Persistwise saves a full copy of the page (text, styles,
images, icons and fonts, in one self-contained file that opens offline).
Follow blogs and newsletters and every new post is saved automatically.
Browse and search everything you saved. Pages also go to the Wayback Machine
(on by default) and, if you set it up, to IPFS. No account, no Nine Point Labs servers,
no telemetry.

Linux is free. Windows will be a paid edition through the Microsoft Store.
There is no macOS version.

> Status: v0.1, under review. Nothing has been released yet; the install steps
> below describe how releases will be published.

## Install on Linux

Works on any x86_64 Linux distro (built on Ubuntu 22.04, glibc 2.35+).

### Omarchy

Download `persistwise-desktop-<version>-x86_64.AppImage` from the
[releases page](https://github.com/ninepointlabs/persistwise-desktop/releases), then:

```sh
chmod +x persistwise-desktop-*-x86_64.AppImage
./persistwise-desktop-*-x86_64.AppImage --install-desktop-entry   # adds it to Walker and the dock
```

Persistwise follows your Omarchy theme and updates live when you switch themes.

Or install it with [autonomix-cli](https://github.com/ninepointlabs/autonomix-cli):

```sh
autonomix-cli add https://github.com/ninepointlabs/persistwise-desktop
```

### Other distros

* **AppImage**: download, `chmod +x`, run. To add it to your app menu, use
  [Gear Lever](https://flathub.org/apps/it.mijorus.gearlever) or AppImageLauncher,
  or run it once with `--install-desktop-entry` (`--remove-desktop-entry` undoes it).
  Gear Lever and AppImageUpdate can update it in place.
* **Flatpak bundle** (self-hosted, not on Flathub): download
  `persistwise-desktop-<version>-x86_64.flatpak` and run
  `flatpak install --user ./persistwise-desktop-<version>-x86_64.flatpak`.
  It needs the GNOME 50 runtime from Flathub, which flatpak offers to install.
* **autonomix-cli**: `autonomix-cli add https://github.com/ninepointlabs/persistwise-desktop`
  ([autonomix-cli](https://github.com/ninepointlabs/autonomix-cli)).

If the AppImage does not start and mentions FUSE, either install FUSE 2
(`libfuse2` on Ubuntu/Debian, `fuse-libs` on Fedora, `fuse2` on Arch) or run it
with `--appimage-extract-and-run`.

Without Omarchy, Persistwise uses its own light or dark look, following your
system's preference.

## Install on Windows

Windows is the paid edition and will be offered through the Microsoft Store.
It is not in the Store yet. Windows 10 (1809 or later) or Windows 11, with the
Microsoft Edge WebView2 runtime (built into Windows 11).

## How archiving works

* **Local copy (always).** The page is downloaded, then its stylesheets,
  images, icons and fonts are downloaded and embedded, so the saved file works
  with no network. Scripts are removed: a saved copy is a static snapshot.
  PDFs, images and other non-HTML links are kept as they are.
* **Viewing.** Saved copies open inside the app in a sandbox: no scripts, no
  forms, and links are inactive (use "Open original").
* **Search.** Titles, addresses, page text and feed names are indexed with
  SQLite full-text search. Search works offline.
* **Wayback Machine (on by default).** Unless you untick "Also send to the
  Wayback Machine" (or turn it off in Settings), Persistwise asks the Internet Archive's free,
  public [Save Page Now](https://web.archive.org/save) to capture the page and
  records the snapshot link. The page's address is sent to the Internet Archive
  and the capture is public. Save Page Now is often busy or limits how often a
  page can be captured; if it declines because the page was captured recently,
  Persistwise links the newest existing snapshot. A failed backup never affects
  the local copy, and you can try again from the page.
* **Where it lives.** `~/.local/share/persistwise` on Linux
  (`~/.var/app/com.ninepointlabs.Persistwise/data/persistwise` for the Flatpak),
  `%APPDATA%\Nine Point Labs\Persistwise\data` on Windows. Back up that folder
  to back up everything. `PERSISTWISE_DATA_DIR` overrides it.

## IPFS (optional)

Off by default. Persistwise never runs or pays for pinning; you pick one in
Settings:

* **Your own node**: install [IPFS Desktop](https://docs.ipfs.tech/install/ipfs-desktop/)
  or [Kubo](https://docs.ipfs.tech/install/command-line/) and keep it running.
  Persistwise talks to its RPC API at `http://127.0.0.1:5001` and pins each
  saved file there. Any hosted Kubo-compatible RPC also works with your own token.
* **Your own Pinata key**: create a key in your
  [Pinata account](https://app.pinata.cloud/developers/api-keys) and paste the JWT.
  Uploads count against your Pinata plan.

Use "Test connection" to check it. Keys stay on your computer in
`secrets.json` in the data folder (readable only by you) and are only ever
shown as their last four characters. If IPFS is off, missing or broken, saving
and browsing work exactly the same.

## Feeds

Follow a blog, newsletter or social feed (RSS, Atom or JSON Feed) with **+**
under Feeds. You can paste the feed address or just the site's address.
Persistwise checks your feeds every 15 minutes while it is open (change it in
Settings) and saves a full copy of every new post, sends it to the Wayback
Machine, and pins it to IPFS if you set that up. The first check of a new feed
saves every post the feed currently lists. Downloads that fail are retried on
later checks; Wayback and IPFS failures are retried about hourly.

**When Persistwise is closed:** on Linux, turn on Settings → "Keep checking
feeds when Persistwise is closed". That installs a systemd *user* timer on your
computer (no root) that runs `persistwise-desktop --poll` on the same
schedule; turning it off removes it. If you move the AppImage, turn the setting
off and on again. Without systemd, run `persistwise-desktop --poll` from cron.
On Windows the same setting (on by default) keeps Persistwise in the tray
when you close its window and starts it hidden when you sign in; right-click
the tray icon to quit. The Flatpak checks feeds while it is open.

## Moving from the Persistwise website

A one-time step, not an ongoing sync: run the read-only script
`tools/web-export/export_archive.rb` once against the web app to get an export
zip, then choose **Import from Persistwise web** in the sidebar (or run
`persistwise-desktop --import persistwise-export.zip`). Your feeds, posts,
Wayback and IPFS links and saved copies come over, and the desktop app carries
on checking the feeds. The website stored pages without their images, so
on each feed check Persistwise also fetches and embeds the images and styles
of a batch of imported pages (or use "Embed images now" on a page); after
that they open fully offline. Importing a fresher export again later updates
what changed without duplicates. Steps: [docs/web-migration.md](docs/web-migration.md);
format: [docs/import-format.md](docs/import-format.md).

## Known limits (v0.1)

* Pages that build their content with JavaScript after loading may save
  incompletely, because the copy is made from the HTML the site sends, not from
  a rendered browser tab. Pages behind a login are saved as a logged-out visitor sees them.
* Video and audio are linked, not embedded. Very large pages are capped (25 MB
  page, 8 MB per resource, 60 MB per save).
* Imported copies need the network for their images until Persistwise has
  embedded them (a batch per feed check), and images that have since vanished
  from the original site cannot be recovered.
* No mobile app yet.

## Build from source

Needs Rust (stable), Node.js (for the Tauri CLI only) and, on Linux, the
WebKitGTK 4.1 development files (`webkit2gtk-4.1` on Arch,
`libwebkit2gtk-4.1-dev` on Debian/Ubuntu).

```sh
npm ci                                   # Tauri CLI
cargo test --workspace                   # tests (no network needed)
npx tauri dev                            # run the app
./scripts/build-appimage.sh              # dist/*.AppImage (+ .zsync, .sha256)
./scripts/build-flatpak.sh               # dist/*.flatpak (needs flatpak)
pwsh scripts/build-msix.ps1              # Windows: dist\*.msix (needs Windows SDK)
```

Other flags: `--version`, `--poll` (check feeds once and exit),
`--import <zip|folder>`, `--self-test` (opens the archive and exits),
`--smoke-test` (opens the window, checks the UI and feed polling, and exits).

## License

MIT. Made by Nine Point Labs.
