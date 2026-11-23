# Persistwise export format (`persistwise-export`, version 1)

This is the file Persistwise Desktop imports to bring over a Persistwise web
account, once. It is made by the one-time script `tools/web-export/export_archive.rb`
(see `docs/web-migration.md`); the web app itself has no export feature.

A working example is in `crates/core/tests/fixtures/export-v1/`.

## Container

A `.zip` file (or the same files in a plain folder):

```text
manifest.json
saved-copies/<archive_record_id>.html
```

The files may also sit inside one top-level folder in the zip
(`persistwise-export-2026-11-23/manifest.json`); the importer finds them.

Paths in the manifest are relative, use `/`, and may not contain `..`, a
leading `/` or a backslash. The importer only ever reads entries the manifest
names, so stray or hostile zip entries are ignored.

## `manifest.json`

```json
{
  "format": "persistwise-export",
  "version": 1,
  "exported_at": "2026-11-23T12:00:00Z",
  "source": { "app": "persistwise-web", "host": "persistwise.com", "app_version": "<git sha>" },
  "feeds": [ Feed, ... ],
  "posts": [ Post, ... ]
}
```

`source.host` scopes the ids: the importer keys records as
`<host>:feed:<id>` and `<host>:post:<id>`. It defaults to `persistwise.com`.

### Feed (one row of the web app's `feeds` table, minus `user_id`)

| field | type | notes |
| --- | --- | --- |
| `id` | integer | web `feeds.id` |
| `url` | string | feed URL |
| `name` | string or null | |
| `kind` | string or null | `blog`, `newsletter`, `social` |
| `status` | string or null | `active`, `paused`, `errored` |
| `last_polled_at` | RFC 3339 or null | |
| `created_at` | RFC 3339 or null | |

`last_error` is deliberately left out (it can contain server details).

### Post (one row of `posts`, with its `archive_records` row and saved copy)

| field | type | notes |
| --- | --- | --- |
| `id` | integer | web `posts.id` |
| `feed_id` | integer or null | web `feeds.id` |
| `guid`, `url`, `title`, `author` | string / null | |
| `published_at`, `fetched_at`, `created_at` | RFC 3339 or null | |
| `status` | string or null | `discovered`, `fetched`, `archiving`, `archived`, `failed` |
| `archive_record` | object or null | see below |
| `saved_copy` | object or null | null when the web app never stored a copy |

`archive_record` (from `archive_records`):

| field | notes |
| --- | --- |
| `id` | web `archive_records.id` (used to name the saved copy) |
| `original_url`, `status` | |
| `wayback_url`, `wayback_submitted_at`, `wayback_failed_at` | |
| `cid`, `pin_service`, `archived_at`, `pin_failed_at` | `pin_service` is `pinata` or `lighthouse` |
| `last_error` | shown on the desktop when a step failed |

`wayback_job_id` is not exported (it is only meaningful to the web app's SPN2 polling).

`saved_copy`:

| field | notes |
| --- | --- |
| `path` | e.g. `saved-copies/101.html` |
| `sha256` | lowercase hex of the file; the importer refuses a mismatch |
| `bytes` | file size |
| `content_type` | `text/html` today |

## What the importer does

* Feeds and posts are upserted by `<host>:<kind>:<id>`. Importing the same or a
  newer export again updates titles, statuses and links, writes saved copies
  whose checksum changed, and adds new posts. Nothing is duplicated.
* Wayback/IPFS fields are copied as recorded: a `wayback_url` shows as
  "Wayback ✓", a `wayback_failed_at` without a URL as "Wayback failed" with
  `last_error`; the same for `cid` / `pin_failed_at`.
* Saved copies are indexed for full-text search.
* One bad post (checksum mismatch, unsafe path) is skipped and reported; the
  rest of the import continues. A missing file is counted, not fatal.
* The desktop app never deletes local pages because they vanished from a newer
  export.

Run it from the app (Import from Persistwise web) or headless:

```sh
persistwise-desktop --import ~/Downloads/persistwise-export.zip
```

which prints a JSON report and exits 0 (all good), 3 (some posts skipped) or 1 (failed).
