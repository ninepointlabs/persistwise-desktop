# Moving from the Persistwise website to the desktop app (one time)

This is a one-time migration, not a feature of either app. You run one
read-only script against the web app's database and files. It writes a
`persistwise-export` zip ([format](import-format.md)), and Persistwise Desktop
imports it. After that, the desktop app polls your feeds itself and the
website can be retired.

Nothing in the web app's code changes: no new routes, gems, buttons or
migrations.

## 1. Make the export

The script is `tools/web-export/export_archive.rb` in this repo. It needs the
web app's code, its production database and its `storage/archives/` folder,
because the saved copies live on disk there. It only reads.

```sh
# from the web app's directory, where it can reach the production DB and storage/archives
PERSISTWISE_EXPORT_EMAIL=you@example.com \
PERSISTWISE_EXPORT_OUT=tmp/persistwise-export.zip \
bin/rails runner /path/to/persistwise-desktop/tools/web-export/export_archive.rb
```

It prints something like
`persistwise export: 12 feeds, 840 posts, 838 saved copies (2 listed but missing on disk) -> tmp/persistwise-export.zip`.

Only that one account's feeds, posts, archive records and saved copies are
included. Passwords, 2FA, sessions, Stripe data and `feeds.last_error` are
left out.

### On Railway (suggested steps; not run by the desktop work)

The production app runs on Railway with `storage/` on its volume. One way to do it:

```sh
railway ssh                                   # into the web service
# paste the script into tmp/export_archive.rb (e.g. with cat > tmp/export_archive.rb), then:
PERSISTWISE_EXPORT_EMAIL=you@example.com bin/rails runner tmp/export_archive.rb
base64 -w0 tmp/persistwise-export.zip         # copy the output, or stream it:
```

From your computer, `railway ssh -- base64 -w0 tmp/persistwise-export.zip | base64 -d > persistwise-export.zip`
avoids terminal mangling of binary data. Check the result with `unzip -t persistwise-export.zip`.
Delete `tmp/persistwise-export.zip` and `tmp/export_archive.rb` on the server afterwards.

## 2. Import it

In Persistwise Desktop choose **Import from Persistwise web** and pick the zip, or run:

```sh
persistwise-desktop --import persistwise-export.zip
```

Feeds come in already followed, so the desktop app starts checking them. Posts whose
page the website never managed to save are downloaded on the next check.
Running the import again (for example with a fresher export just before you
switch the website off) updates what changed and adds what is new, with no
duplicates. Copies the desktop app saved itself (with images embedded) are kept
in preference to the website's HTML.

## How this was tested

`export_archive.rb` was run under the web app's own code (master @ 703caff,
Ruby 4.0.6) against a fresh development database filled with fake data
(two users, three feeds, five posts, including a missing file and a post that
was never fetched), in a throwaway copy that was deleted afterwards. The zip
passed `unzip -t`, contained only the chosen user's data, and imported into
the desktop app twice with no duplicates.
