# One-time migration: export a Persistwise web account for Persistwise Desktop.
#
# This is not a web-app feature. It is a script you run once, with the web
# app's own code and database, to produce a `persistwise-export` v1 zip
# (docs/import-format.md) that Persistwise Desktop imports. It only reads:
# it changes no records and needs no new gems or routes.
#
# Usage (from the web app's directory, where its database and
# storage/archives live):
#
#   PERSISTWISE_EXPORT_EMAIL=you@example.com \
#   PERSISTWISE_EXPORT_OUT=tmp/persistwise-export.zip \
#   bin/rails runner /path/to/export_archive.rb
#
# It prints a one-line summary to stderr. Then copy the zip to your computer
# and choose it in Persistwise Desktop (Import from Persistwise web), or run
# `persistwise-desktop --import persistwise-export.zip`.
#
# Pure Ruby standard library (Zlib for CRC-32 and deflate); writes a normal
# zip readable by any unzip tool.

require "json"
require "zlib"
require "digest"
require "time"

module PersistwiseExport
  # Minimal zip writer (deflate, no zip64; entries and archive must stay
  # under 4 GB, which saved HTML pages do by a wide margin).
  class ZipWriter
    Entry = Struct.new(:name, :crc, :csize, :size, :offset, :dos_time, :dos_date)

    def initialize(io)
      @io = io
      @entries = []
      @offset = 0
    end

    def add(name, data)
      data = data.b
      deflated = Zlib::Deflate.new(Zlib::BEST_COMPRESSION, -Zlib::MAX_WBITS).then { |z| z.deflate(data, Zlib::FINISH).tap { z.close } }
      crc = Zlib.crc32(data)
      t = Time.now
      dos_time = (t.hour << 11) | (t.min << 5) | (t.sec / 2)
      dos_date = ((t.year - 1980) << 9) | (t.month << 5) | t.day
      entry = Entry.new(name.b, crc, deflated.bytesize, data.bytesize, @offset, dos_time, dos_date)
      header = [0x04034b50, 20, 0x0800, 8, dos_time, dos_date, crc, deflated.bytesize, data.bytesize, entry.name.bytesize, 0].pack("VvvvvvVVVvv")
      write(header + entry.name + deflated)
      @entries << entry
    end

    def finish
      cd_start = @offset
      @entries.each do |e|
        write([0x02014b50, 20, 20, 0x0800, 8, e.dos_time, e.dos_date, e.crc, e.csize, e.size, e.name.bytesize, 0, 0, 0, 0, 0, e.offset].pack("VvvvvvvVVVvvvvvVV") + e.name)
      end
      cd_size = @offset - cd_start
      write([0x06054b50, 0, 0, @entries.size, @entries.size, cd_size, cd_start, 0].pack("VvvvvVVv"))
    end

    private
      def write(bytes)
        @io.write(bytes)
        @offset += bytes.bytesize
      end
  end

  module_function

  def iso(t)
    t&.utc&.iso8601
  end

  def run(email:, out:, host:)
    user = User.find_by!(email_address: email.strip.downcase)
    feeds = Feed.where(user_id: user.id).order(:id).to_a
    posts = Post.where(feed_id: feeds.map(&:id)).includes(:archive_record).order(:id).to_a

    copies = {}
    missing = 0
    post_rows = posts.map do |post|
      record = post.archive_record
      saved = nil
      path = record&.saved_copy_path
      if record && path
        data = File.binread(path)
        name = "saved-copies/#{record.id}.html"
        copies[name] = data
        saved = { path: name, sha256: Digest::SHA256.hexdigest(data), bytes: data.bytesize, content_type: "text/html" }
      elsif post.local_path.present?
        missing += 1
      end
      {
        id: post.id, feed_id: post.feed_id, guid: post.guid, url: post.url, title: post.title, author: post.author,
        published_at: iso(post.published_at), fetched_at: iso(post.fetched_at), created_at: iso(post.created_at),
        status: post.status,
        archive_record: record && {
          id: record.id, original_url: record.original_url, status: record.status,
          wayback_url: record.wayback_url.presence, wayback_submitted_at: iso(record.wayback_submitted_at),
          wayback_failed_at: iso(record.wayback_failed_at), cid: record.cid.presence, pin_service: record.pin_service.presence,
          archived_at: iso(record.archived_at), pin_failed_at: iso(record.pin_failed_at), last_error: record.last_error
        },
        saved_copy: saved
      }
    end

    manifest = {
      format: "persistwise-export",
      version: 1,
      exported_at: Time.now.utc.iso8601,
      source: { app: "persistwise-web", host: host, app_version: ENV["RAILWAY_GIT_COMMIT_SHA"].presence },
      feeds: feeds.map do |f|
        { id: f.id, url: f.url, name: f.name, kind: f.kind, status: f.status,
          last_polled_at: iso(f.last_polled_at), created_at: iso(f.created_at) }
      end,
      posts: post_rows
    }

    File.open("#{out}.part", "wb") do |io|
      zip = ZipWriter.new(io)
      zip.add("manifest.json", JSON.pretty_generate(manifest))
      copies.each { |name, data| zip.add(name, data) }
      zip.finish
    end
    File.rename("#{out}.part", out)
    warn "persistwise export: #{feeds.size} feeds, #{posts.size} posts, #{copies.size} saved copies " \
         "(#{missing} listed but missing on disk) -> #{out} (#{File.size(out)} bytes)"
  end
end

if defined?(Rails)
  email = ENV.fetch("PERSISTWISE_EXPORT_EMAIL") { abort "Set PERSISTWISE_EXPORT_EMAIL to the account's email address." }
  out = ENV.fetch("PERSISTWISE_EXPORT_OUT", Rails.root.join("tmp", "persistwise-export.zip").to_s)
  host = ENV.fetch("PERSISTWISE_EXPORT_HOST", "persistwise.com")
  PersistwiseExport.run(email: email, out: out, host: host)
end
