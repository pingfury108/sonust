use std::path::Path;

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

const MIGRATION: &str = r#"
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS artists (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS albums (
    id        INTEGER PRIMARY KEY,
    artist_id INTEGER NOT NULL REFERENCES artists(id),
    name      TEXT NOT NULL,
    year      INTEGER,
    UNIQUE(artist_id, name)
);
CREATE TABLE IF NOT EXISTS tracks (
    id        INTEGER PRIMARY KEY,
    folder    INTEGER NOT NULL DEFAULT 0,
    album_id  INTEGER REFERENCES albums(id),
    artist_id INTEGER REFERENCES artists(id),
    title     TEXT NOT NULL,
    track_no  INTEGER,
    year      INTEGER,
    genre     TEXT,
    duration  REAL,
    format    TEXT,
    path      TEXT NOT NULL,
    mtime     INTEGER NOT NULL,
    size      INTEGER NOT NULL,
    has_cover INTEGER NOT NULL DEFAULT 0,
    UNIQUE(folder, path)
);
CREATE INDEX IF NOT EXISTS idx_tracks_album  ON tracks(album_id);
CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist_id);
CREATE INDEX IF NOT EXISTS idx_tracks_title  ON tracks(title);
CREATE TABLE IF NOT EXISTS starred (
    item_type TEXT NOT NULL,
    item_id   INTEGER NOT NULL,
    created   TEXT NOT NULL,
    PRIMARY KEY(item_type, item_id)
);
CREATE TABLE IF NOT EXISTS plays (
    id         INTEGER PRIMARY KEY,
    track_id   INTEGER NOT NULL REFERENCES tracks(id),
    played_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_plays_track ON plays(track_id);
CREATE INDEX IF NOT EXISTS idx_plays_time  ON plays(played_at);
CREATE TABLE IF NOT EXISTS playlists (
    id       INTEGER PRIMARY KEY,
    name     TEXT NOT NULL,
    comment  TEXT,
    owner    TEXT,
    public   INTEGER NOT NULL DEFAULT 0,
    created  TEXT NOT NULL,
    changed  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS playlist_items (
    id          INTEGER PRIMARY KEY,
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    INTEGER NOT NULL,
    position    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_playlist_items_pl ON playlist_items(playlist_id, position);
CREATE TABLE IF NOT EXISTS mirror_files (
    track_id  INTEGER PRIMARY KEY,
    src_size  INTEGER NOT NULL,
    src_mtime INTEGER NOT NULL,
    created   TEXT NOT NULL
);
"#;

pub async fn init(data_dir: &Path) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::new()
        .filename(data_dir.join("sonust.db"))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        // 并发请求 + 后台扫描下的锁竞争等待，避免 database is locked
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await?;
    sqlx::raw_sql(MIGRATION).execute(&pool).await?;
    Ok(pool)
}

pub async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>> {
    let row = sqlx::query("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.get("value")))
}

pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query("INSERT INTO settings(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn upsert_artist(pool: &SqlitePool, name: &str) -> Result<i64> {
    sqlx::query("INSERT INTO artists(name) VALUES(?) ON CONFLICT(name) DO NOTHING")
        .bind(name)
        .execute(pool)
        .await?;
    let row = sqlx::query("SELECT id FROM artists WHERE name = ?")
        .bind(name)
        .fetch_one(pool)
        .await?;
    Ok(row.get("id"))
}

pub async fn upsert_album(
    pool: &SqlitePool,
    artist_id: i64,
    name: &str,
    year: Option<i64>,
) -> Result<i64> {
    sqlx::query(
        "INSERT INTO albums(artist_id, name, year) VALUES(?, ?, ?)
         ON CONFLICT(artist_id, name) DO UPDATE SET year = COALESCE(excluded.year, albums.year)",
    )
    .bind(artist_id)
    .bind(name)
    .bind(year)
    .execute(pool)
    .await?;
    let row = sqlx::query("SELECT id FROM albums WHERE artist_id = ? AND name = ?")
        .bind(artist_id)
        .bind(name)
        .fetch_one(pool)
        .await?;
    Ok(row.get("id"))
}

/// item_type: "track" / "album" / "artist"
pub async fn set_starred(pool: &SqlitePool, item_type: &str, item_id: i64, starred: bool) -> Result<()> {
    if starred {
        sqlx::query(
            "INSERT INTO starred(item_type, item_id, created) VALUES(?, ?, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(item_type, item_id) DO NOTHING",
        )
        .bind(item_type)
        .bind(item_id)
        .execute(pool)
        .await?;
    } else {
        sqlx::query("DELETE FROM starred WHERE item_type = ? AND item_id = ?")
            .bind(item_type)
            .bind(item_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// 清理无曲目的专辑与无专辑的艺术家。
pub async fn prune_empty_albums_artists(pool: &SqlitePool) -> Result<()> {
    sqlx::query("DELETE FROM albums WHERE id NOT IN (SELECT DISTINCT album_id FROM tracks WHERE album_id IS NOT NULL)")
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM artists WHERE id NOT IN (SELECT DISTINCT artist_id FROM albums) AND id NOT IN (SELECT DISTINCT artist_id FROM tracks WHERE artist_id IS NOT NULL)")
        .execute(pool)
        .await?;
    Ok(())
}

/// 已存在且指纹未变时返回 Ok(true)，跳过。
pub async fn track_unchanged(pool: &SqlitePool, folder: i64, path: &str, mtime: i64, size: i64) -> Result<bool> {
    let row = sqlx::query("SELECT mtime, size FROM tracks WHERE folder = ? AND path = ?")
        .bind(folder)
        .bind(path)
        .fetch_optional(pool)
        .await?;
    Ok(matches!(row, Some(r) if r.get::<i64, _>("mtime") == mtime && r.get::<i64, _>("size") == size))
}

#[allow(clippy::too_many_arguments)]
pub async fn upsert_track(
    pool: &SqlitePool,
    folder: i64,
    path: &str,
    title: &str,
    artist_id: Option<i64>,
    album_id: Option<i64>,
    track_no: Option<i64>,
    year: Option<i64>,
    genre: Option<&str>,
    duration: f64,
    format: &str,
    mtime: i64,
    size: i64,
    has_cover: bool,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO tracks(folder, path, title, artist_id, album_id, track_no, year, genre, duration, format, mtime, size, has_cover)
         VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(folder, path) DO UPDATE SET
           title = excluded.title, artist_id = excluded.artist_id, album_id = excluded.album_id,
           track_no = excluded.track_no, year = excluded.year, genre = excluded.genre,
           duration = excluded.duration, format = excluded.format,
           mtime = excluded.mtime, size = excluded.size, has_cover = excluded.has_cover",
    )
    .bind(folder)
    .bind(path)
    .bind(title)
    .bind(artist_id)
    .bind(album_id)
    .bind(track_no)
    .bind(year)
    .bind(genre)
    .bind(duration)
    .bind(format)
    .bind(mtime)
    .bind(size)
    .bind(has_cover)
    .execute(pool)
    .await?;
    Ok(())
}
