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

> Status: v0.1.2 is out on the
> [releases page](https://github.com/ninepointlabs/persistwise-desktop/releases).

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
* **Debian / Ubuntu** (22.04 or newer): download
  `persistwise-desktop_<version>_amd64.deb` and run
  `sudo apt install ./persistwise-desktop_<version>_amd64.deb`.
* **Fedora / RHEL-family**: download `persistwise-desktop-<version>-1.x86_64.rpm`
  and run `sudo dnf install ./persistwise-desktop-<version>-1.x86_64.rpm`.
* **Arch**: download `persistwise-desktop-bin-<version>-1-x86_64.pkg.tar.zst` and run
  `sudo pacman -U ./persistwise-desktop-bin-<version>-1-x86_64.pkg.tar.zst`,
  or download `PKGBUILD` into an empty folder and run `makepkg -si`.
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
* **Where it lives.** One folder holds everything (see
  [Where your archive lives](#where-your-archive-lives)); back up that folder
  to back up everything.

## Saving a whole forum thread

Tick **Save the whole thread** next to the address before pressing Save.
Persistwise saves the page, then follows the thread's next-page links and saves
every page, each as its own full offline copy, and shows them as **one item**
in your archive (title, address, number of pages, date). Open it to read the
pages in order, with Previous page / Next page and a page picker. Search finds
text on any page and opens the right page. Plain Save (box unticked) still
saves just the one page.

* **How the next page is found.** A `Link: …; rel="next"` header,
  `<link rel="next">` or `<a rel="next">` first; then a link that says "Next"
  (by its text, title or class) and stays in the same thread; then the next
  page number in a common form (`?page=2`, `&start=15`, `&st=30`, `/page-2`,
  `/page/2`). That covers typical phpBB, vBulletin, XenForo, MyBB, Discourse,
  old Invision Power Board and WordPress-style pagination without anything
  site-specific. `http://` links on an `https://` page of the same site are
  followed over https, and a session id in the address (`s=…`, `sid=…`,
  `PHPSESSID=…`) does not make the same page look like a different one.
* **Limits.** Only pages on the same site are followed. It stops when there is
  no next page, when a link leads back to a page already saved in this run,
  or after **100 pages**; then the item is marked *partial thread*. Pages are
  fetched one at a time with a short pause between them. The usual size caps
  apply to each page.
* **If a page fails**, the pages already saved are kept and the item is
  marked partial, with the reason.
* **Not a thread?** If no next page is found, the one page is saved as a
  normal page and Persistwise says it did not look like a multi-page thread.
* **Saving again** (from the first page's address or any other page of the
  thread) continues the same item instead of starting a new one. The earlier
  pages are reused as they are, without being downloaded again. The **last
  saved page is downloaded again** and its copy replaced if it changed, so
  new replies on it are kept, and any pages that have appeared after it are
  added to the end, in order, under the same limits. No page is stored twice.
  If the last page cannot be reached, nothing changes and Persistwise says
  so; if a new page fails or the 100-page limit is reached, the pages added
  so far are kept and the item is marked partial.
* **Wayback and IPFS** work as for a single page, for every page saved or
  updated by that save: Wayback when "Also send to the Wayback Machine" is
  ticked (pages already captured are not sent again), IPFS when pin-on-save is
  on (an updated last page is pinned again, because its content changed).
  They run one page at a time after the save.
* **Old IPFS pins are removed.** When the last page's copy is replaced, the
  pin of its earlier copy is removed from your node or Pinata, unless another
  saved page has the same content. If that fails (node not running, IPFS now
  off or set to the other service, a key without delete rights), the save
  still completes and its message names the CID so you can remove it by hand.

## Where your archive lives

By default:

| | Folder |
| --- | --- |
| Linux | `~/.local/share/persistwise` (respects `XDG_DATA_HOME`) |
| Flatpak | `~/.var/app/com.ninepointlabs.Persistwise/data/persistwise` |
| Windows | `%APPDATA%\Nine Point Labs\Persistwise\data` |

To keep it somewhere else (another disk, a synced folder), open
**Settings → Storage → Choose a different folder…** and pick a folder:

* A folder that **already holds a Persistwise archive** is opened as it is.
  Nothing is merged or overwritten, and your current archive stays where it is.
* An **empty** folder offers three choices: **Move** your archive there
  (recommended: one archive; the old copy is removed only after the new one
  is checked, and a small `persistwise-moved.json` note is left behind so a
  later start finds the new folder), **Copy** it there (both stay usable;
  Persistwise uses the new one), or **Start fresh** (an empty archive; the
  old folder, including your settings and keys, is left alone). Start fresh
  then asks whether to **keep your settings and API keys** (your Wayback,
  feed-checking and IPFS settings and saved keys are copied into the new
  archive; pages, feeds and the library are not) or **start with fresh
  settings** (defaults, no keys).
* A folder that **already has other files in it** (your Documents folder, say)
  is not used directly: Persistwise offers to create a `Persistwise` folder
  inside it and use that, with the same choices as above (or, if that
  `Persistwise` folder already holds an archive, to open it). Nothing is
  created until you choose.
* The current archive folder, a folder inside it or containing it, a file,
  and a folder Persistwise cannot write to are refused, with the reason.

**Use the default folder** in the same place goes back. The app switches over
straight away; if "Keep checking feeds when Persistwise is closed" is on, its
timer is pointed at the new folder.

Because `settings.json` lives inside the archive, the chosen folder is
remembered in a small file outside it: `~/.config/persistwise/archive-location.json`
on Linux (respects `XDG_CONFIG_HOME`; Flatpak:
`~/.var/app/com.ninepointlabs.Persistwise/config/persistwise/`), and
`%APPDATA%\Nine Point Labs\Persistwise\config\archive-location.json` on
Windows. Delete that file to go back to the default folder.

**If the chosen folder is missing when Persistwise starts** (an unplugged
drive, an unmounted share), the window opens on a screen that says so, with
three choices: **Retry** (after reconnecting the drive), **Choose a different
folder…** (same rules as in Settings), or **Use the default folder** (opens
the archive there, or starts a new, empty one only if you confirm). Nothing is
created anywhere until you choose, and the missing archive is never changed.
Without a window (`--poll` from the background timer, `--import`,
`--self-test`) Persistwise prints the problem and exits instead. When
`PERSISTWISE_DATA_DIR` is set, it is used as before, with no recovery screen.

The folder is decided in this order, for the window and for `--poll`,
`--import`, `--self-test` and `--smoke-test` alike:

1. `PERSISTWISE_DATA_DIR`, if set (portable use, tests). While it is set the
   folder cannot be changed in Settings.
2. The folder chosen in Settings.
3. The default above.

**Flatpak:** the Flatpak has no access to your home folder. A folder you pick
is shared with it through the desktop's file-chooser portal (the document
portal), which remembers the grant across restarts; Persistwise stores the
portal's path for it (under `/run/user/<uid>/doc/…`). If that grant is ever
removed (for example with `flatpak permission-reset com.ninepointlabs.Persistwise`),
Persistwise shows the folder as unavailable at start; use **Choose a
different folder…** there and pick the same folder again to renew the grant.

## IPFS (optional)

Off by default. Persistwise never runs or pays for pinning; you pick one in
Settings:

* **Your own node**: install [IPFS Desktop](https://docs.ipfs.tech/install/ipfs-desktop/)
  or [Kubo](https://docs.ipfs.tech/install/command-line/) and keep it running.
  Persistwise talks to its RPC API at `http://127.0.0.1:5001` and pins each
  saved file there. Any hosted Kubo-compatible RPC also works with your own token.
* **Your own Pinata key**: create a key in your
  [Pinata account](https://app.pinata.cloud/developers/api-keys) and paste the JWT.
  Uploads count against your Pinata plan. Give the key permission to read
  and write files: Persistwise uploads with it, and deletes the earlier copy
  of a page it re-saves in a thread.

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
* Thread saving follows next-page links in the HTML the site sends. Forums
  that only load later pages with JavaScript (infinite scroll with no page
  links) save as a single page.
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
./scripts/build-linux-packages.sh        # dist/*.deb, *.rpm, PKGBUILD (needs dpkg-dev, rpm)
pwsh scripts/build-msix.ps1              # Windows: dist\*.msix (needs Windows SDK)
```

Other flags: `--version`, `--poll` (check feeds once and exit),
`--import <zip|folder>`, `--self-test` (opens the archive and exits),
`--smoke-test` (opens the window, checks the UI and feed polling, and exits).

## License

MIT. Made by Nine Point Labs.
