use std::path::{Path, PathBuf};

use anyhow::Result;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::Accessor;
use sqlx::{Row, SqlitePool};
use tracing::{info, warn};
use walkdir::WalkDir;

const AUDIO_EXTS: &[&str] = &["mp3", "flac", "m4a", "wav", "ogg", "opus", "aac", "wma", "aiff"];

#[derive(Debug, Default)]
pub struct ScanStats {
    pub scanned: usize,
    pub updated: usize,
    pub skipped: usize,
    pub failed: usize,
    pub pruned: usize,
}

struct TrackMeta {
    folder: i64,
    rel_path: String,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    track_no: Option<i64>,
    year: Option<i64>,
    genre: Option<String>,
    duration: f64,
    format: String,
    mtime: i64,
    size: i64,
    has_cover: bool,
}

fn parse_tags(full: &Path, folder: i64, rel_path: String) -> Result<TrackMeta> {
    let meta = std::fs::metadata(full)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let size = meta.len() as i64;
    let format = full
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let tagged = Probe::open(full)?.read()?;
    let duration = tagged.properties().duration().as_secs_f64();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let fallback_title = || {
        full.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Unknown".into())
    };

    let (title, artist, album, track_no, year, genre, has_cover) = match tag {
        Some(t) => (
            t.title().map(|s| s.into_owned()).unwrap_or_else(fallback_title),
            // 优先 album_artist，保证合辑归到正确的艺术家下
            t.get_string(&lofty::tag::ItemKey::AlbumArtist)
                .map(|s| s.to_string())
                .or_else(|| t.artist().map(|s| s.into_owned()))
                .filter(|s| !s.is_empty()),
            t.album().map(|s| s.into_owned()),
            t.track().map(|n| n as i64),
            t.year().map(|n| n as i64),
            t.genre().map(|s| s.into_owned()),
            !t.pictures().is_empty(),
        ),
        None => (fallback_title(), None, None, None, None, None, false),
    };

    Ok(TrackMeta {
        folder,
        rel_path,
        title,
        artist,
        album,
        track_no,
        year,
        genre,
        duration,
        format,
        mtime,
        size,
        has_cover,
    })
}

fn collect(music_dirs: &[PathBuf]) -> Vec<(PathBuf, i64, String)> {
    let mut files = Vec::new();
    for (idx, dir) in music_dirs.iter().enumerate() {
        for entry in WalkDir::new(dir).follow_links(false) {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!(error = %e, "walk error");
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let is_audio = entry
                .path()
                .extension()
                .map(|e| AUDIO_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
                .unwrap_or(false);
            if !is_audio {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(dir)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            files.push((entry.path().to_path_buf(), idx as i64, rel));
        }
    }
    files
}

pub async fn scan_all(pool: &SqlitePool, music_dirs: &[PathBuf]) -> Result<ScanStats> {
    let dirs = music_dirs.to_vec();
    let files = tokio::task::spawn_blocking(move || collect(&dirs)).await?;

    let mut stats = ScanStats {
        scanned: files.len(),
        ..Default::default()
    };

    for (full, folder, rel) in files {
        let (mtime, size) = std::fs::metadata(&full)
            .ok()
            .map(|m| {
                let mt = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                (mt, m.len() as i64)
            })
            .unwrap_or((0, 0));

        if crate::db::track_unchanged(pool, folder, &rel, mtime, size).await? {
            stats.skipped += 1;
            continue;
        }

        let full_c = full.clone();
        let rel_c = rel.clone();
        let parsed = tokio::task::spawn_blocking(move || parse_tags(&full_c, folder, rel_c)).await?;

        match parsed {
            Ok(t) => {
                let artist_id = match &t.artist {
                    Some(name) => Some(crate::db::upsert_artist(pool, name).await?),
                    None => None,
                };
                let album_id = match (&t.album, artist_id) {
                    (Some(name), Some(aid)) => {
                        Some(crate::db::upsert_album(pool, aid, name, t.year).await?)
                    }
                    _ => None,
                };
                crate::db::upsert_track(
                    pool,
                    t.folder,
                    &t.rel_path,
                    &t.title,
                    artist_id,
                    album_id,
                    t.track_no,
                    t.year,
                    t.genre.as_deref(),
                    t.duration,
                    &t.format,
                    t.mtime,
                    t.size,
                    t.has_cover,
                )
                .await?;
                stats.updated += 1;
            }
            Err(e) => {
                warn!(path = %rel, error = %e, "tag parse failed");
                stats.failed += 1;
            }
        }

        if stats.updated % 500 == 0 && stats.updated > 0 {
            info!(updated = stats.updated, "scanning...");
        }
    }

    // 清理磁盘上已不存在的曲目
    let rows = sqlx::query("SELECT id, folder, path FROM tracks")
        .fetch_all(pool)
        .await?;
    for r in rows {
        let id: i64 = r.get("id");
        let folder: i64 = r.get("folder");
        let path: String = r.get("path");
        let exists = music_dirs
            .get(folder as usize)
            .map(|base| base.join(&path).exists())
            .unwrap_or(false);
        if !exists {
            // 先清依赖行（plays 有 FK，其余是逻辑清理），再删曲目
            sqlx::query("DELETE FROM plays WHERE track_id = ?")
                .bind(id)
                .execute(pool)
                .await?;
            sqlx::query("DELETE FROM playlist_items WHERE track_id = ?")
                .bind(id)
                .execute(pool)
                .await?;
            sqlx::query("DELETE FROM starred WHERE item_type = 'track' AND item_id = ?")
                .bind(id)
                .execute(pool)
                .await?;
            sqlx::query("DELETE FROM tracks WHERE id = ?")
                .bind(id)
                .execute(pool)
                .await?;
            stats.pruned += 1;
        }
    }
    if stats.pruned > 0 {
        crate::db::prune_empty_albums_artists(pool).await?;
    }

    Ok(stats)
}

/// 监听音乐目录变化，防抖后触发增量扫描（含删除清理）。
pub async fn watch(pool: SqlitePool, music_dirs: Vec<PathBuf>) -> Result<()> {
    use notify::{RecursiveMode, Watcher};

    let (tx, mut rx) = tokio::sync::mpsc::channel::<notify::Result<notify::Event>>(100);
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.blocking_send(res);
    })?;
    for dir in &music_dirs {
        watcher.watch(dir, RecursiveMode::Recursive)?;
    }
    info!(dirs = ?music_dirs, "watching music directories");

    const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
    let mut last_scan: Option<std::time::Instant> = None;

    while rx.recv().await.is_some() {
        // 防抖：2 秒静默期内的后续事件全部合并
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        // 限频：持续写入（如下载中的文件）不会让扫描间隔小于 MIN_INTERVAL
        if let Some(t) = last_scan {
            let elapsed = t.elapsed();
            if elapsed < MIN_INTERVAL {
                tokio::time::sleep(MIN_INTERVAL - elapsed).await;
            }
        }
        match scan_all(&pool, &music_dirs).await {
            Ok(s) if s.updated > 0 || s.pruned > 0 => info!(stats = ?s, "rescan after fs change"),
            Ok(_) => {}
            Err(e) => warn!(error = %e, "rescan failed"),
        }
        last_scan = Some(std::time::Instant::now());
    }
    Ok(())
}
