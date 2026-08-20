use std::path::PathBuf;

use anyhow::{Context, Result};
use lofty::file::TaggedFileExt;
use lofty::probe::Probe;
use sqlx::{Row, SqlitePool};

/// 找到某曲目（或专辑第一首曲目）的物理路径。
pub async fn track_path_for(
    pool: &SqlitePool,
    music_dirs: &[PathBuf],
    id: &str,
) -> Result<Option<PathBuf>> {
    let row = if let Some(tid) = crate::subsonic::parse_track_id(id) {
        sqlx::query("SELECT folder, path FROM tracks WHERE id = ?")
            .bind(tid)
            .fetch_optional(pool)
            .await?
    } else if let Some(aid) = crate::subsonic::parse_album_id(id) {
        sqlx::query("SELECT folder, path FROM tracks WHERE album_id = ? AND has_cover = 1 ORDER BY track_no LIMIT 1")
            .bind(aid)
            .fetch_optional(pool)
            .await?
    } else {
        None
    };

    let Some(row) = row else { return Ok(None) };
    let folder: i64 = row.get("folder");
    let path: String = row.get("path");
    let Some(base) = music_dirs.get(folder as usize) else {
        return Ok(None);
    };
    Ok(Some(base.join(path)))
}

/// 提取封面原图（带磁盘缓存）。
async fn original_cover(
    pool: &SqlitePool,
    data_dir: &std::path::Path,
    music_dirs: &[PathBuf],
    id: &str,
) -> Result<Option<PathBuf>> {
    let cache_base = data_dir.join("covers");

    // 命中已有缓存（任意扩展名，排除缩放缓存）
    let mut rd = tokio::fs::read_dir(&cache_base).await?;
    let prefix = format!("{}.", id.replace(':', "-"));
    while let Some(e) = rd.next_entry().await? {
        if e.file_name().to_string_lossy().starts_with(&prefix) {
            return Ok(Some(e.path()));
        }
    }

    let Some(track_path) = track_path_for(pool, music_dirs, id).await? else {
        return Ok(None);
    };

    let tagged = Probe::open(&track_path)
        .with_context(|| format!("open {}", track_path.display()))?
        .read()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let Some(pic) = tag.and_then(|t| t.pictures().first().cloned()) else {
        return Ok(None);
    };

    let ext = match pic.mime_type().map(|m| m.as_str()) {
        Some("image/png") => "png",
        Some("image/gif") => "gif",
        Some("image/webp") => "webp",
        _ => "jpg",
    };
    let out = cache_base.join(format!("{}.{ext}", id.replace(':', "-")));
    tokio::fs::write(&out, pic.data()).await?;
    Ok(Some(out))
}

/// 获取封面；指定 size 时返回等比缩放的 JPEG 缓存（不放大）。
pub async fn cover_file(
    pool: &SqlitePool,
    data_dir: &std::path::Path,
    music_dirs: &[PathBuf],
    id: &str,
    size: Option<u32>,
) -> Result<Option<PathBuf>> {
    let Some(orig) = original_cover(pool, data_dir, music_dirs, id).await? else {
        return Ok(None);
    };
    let Some(size) = size else {
        return Ok(Some(orig));
    };
    let size = size.clamp(16, 2048);
    let out = data_dir
        .join("covers")
        .join(format!("{}_{size}.jpg", id.replace(':', "-")));
    if out.exists() {
        return Ok(Some(out));
    }

    let bytes = tokio::fs::read(&orig).await?;
    let out_c = out.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let img = image::load_from_memory(&bytes)?;
        img.thumbnail(size, size)
            .save_with_format(&out_c, image::ImageFormat::Jpeg)?;
        Ok(())
    })
    .await??;
    Ok(Some(out))
}
