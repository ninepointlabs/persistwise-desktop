# Proposal: account export on the Persistwise web app

Status: proposal only. Nothing in `ninepointlabs/persistwise` was changed.

## What exists today (checked 2026-11-23 against `master` @ `703caff`)

* Data model (`db/schema.rb`): `users` → `feeds` → `posts` → `archive_records`
  (one per post). Saved HTML is on disk at `storage/archives/<feed_id>/<post_id>.html`,
  path in `posts.local_path` (relative to `Rails.root`).
* Saved copies are served only to their owner at `GET /archive/:id/saved-copy`
  (`SavedCopiesController`, sandboxing CSP), where `:id` is the archive record id.
* `config/routes.rb` has no API, no JSON endpoints and no export route.
* The FAQ (`app/models/faq.rb`) answers "Can I export my archive?" with
  "Not yet. There is no export button."

So there is no supported way to get the data out. Scraping the site with a
browser session would work but is fragile and was ruled out.

## Smallest useful change: "Download my archive" (zip)

One button on the Account page that produces exactly the format in
`docs/import-format.md`.

1. **Route**: `resource :archive_export, only: %i[create show]` (POST to start,
   GET to download). Owner only, same `authenticated?` rules as `SavedCopiesController`.
2. **Job**: `ExportArchiveJob.perform_later(user.id)` builds the zip in Solid
   Queue so a large archive does not tie up a web request:
   * `manifest.json` from the user's feeds, posts and archive records
     (field list in `import-format.md`; skip `feeds.last_error` and
     `wayback_job_id`).
   * each saved copy via `ArchiveRecord#saved_copy_path` (already guards
     against paths outside `storage/archives`) into `saved-copies/<record id>.html`,
     with its SHA-256.
   * writes `storage/exports/<user_id>/<random>.zip` (needs the `rubyzip` gem,
     or `zip` via `Open3` if adding a gem is unwanted).
3. **Download**: `GET /account/archive_export` streams the newest zip with
   `send_file`, `Cache-Control: private, no-store`. Delete exports older than
   a day (recurring job), so copies of private data do not pile up.
4. **FAQ**: update the "Can I export my archive?" answer.

Rough size: one controller, one job, one view button, tests for "owner gets
their zip, others get 404, checksums match files". No schema change.

## Later, if live sync is wanted: a read-only token API

For "keep my desktop in sync until I retire the web account" without manual
downloads:

* `personal_access_tokens` table (digest only, `last_used_at`, revocable from
  the Account page), sent as `Authorization: Bearer <token>`.
* `GET /api/v1/export/manifest?since=<rfc3339>` returns the same manifest JSON,
  limited to records changed since the timestamp.
* `GET /api/v1/saved_copies/:archive_record_id` returns the HTML (same CSP
  headers as today) with an `ETag` of its SHA-256.

The desktop importer already keys everything by web id and is idempotent, so
it can consume either the zip or this API without changes to its data model.
The desktop side would add a "Sync now" button that stores the token in its
`secrets.json`.

## What the desktop does not need from the web side

* No Wayback or Pinata credentials: exported links are plain URLs, and the
  desktop app uses public Save Page Now and the user's own IPFS setup.
* No ongoing service: after the last import the desktop works with the web
  account deleted.
