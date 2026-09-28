//! 预转码 MP3 镜像：按 track_id 生成 320k MP3，/stream 在客户端带转码参数时直接回镜像文件。
//! 同步语义：缺失转码、源变更重转、孤儿（源已删）清理、低码率源跳过。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use sqlx::{Row, SqlitePool};
use tracing::{info, warn};

use crate::config::Config;

#[derive(Debug, Default, Clone)]
pub struct MirrorStats {
    /// 库内曲目总数
    pub total: usize,
    /// 值得镜像的曲目数（源码率 >= 目标码率）
    pub eligible: usize,
    /// 镜像就绪（文件存在且指纹新鲜，含本次新转）
    pub covered: usize,
    /// 转码数（实跑=已完成；干跑=预计新转）
    pub transcoded: usize,
    /// 重转数（源 size/mtime 变更）
    pub retranscoded: usize,
    /// 清理数（孤儿 / 不再需要）
    pub removed: usize,
    /// 源码率过低，跳过镜像（直传原文件更小）
    pub skipped_small: usize,
    pub failed: usize,
}

pub fn mirror_path(cfg: &Config, track_id: i64) -> PathBuf {
    cfg.mirror_dir.join(format!("tr-{track_id}.mp3"))
}

fn source_path(cfg: &Config, folder: i64, path: &str) -> Option<PathBuf> {
    cfg.music_dirs
        .get(folder as usize)
        .map(|base| base.join(path))
}

/// /stream 决策：客户端请求转码（format=mp3，或 maxBitRate 低于源码率）
/// 且镜像就绪时返回镜像路径；否则 None 走直传。
pub async fn serve_mirror_path(
    pool: &SqlitePool,
    cfg: &Config,
    track_id: i64,
    q: &HashMap<String, String>,
) -> Option<PathBuf> {
    if !cfg.mirror_enabled {
        return None;
    }
    let format = q.get("format").map(|s| s.to_ascii_lowercase());
    let max_br: i64 = q.get("maxBitRate").and_then(|s| s.parse().ok()).unwrap_or(0);
    if format.as_deref() != Some("mp3") && max_br <= 0 {
        return None;
    }
    let row = sqlx::query("SELECT size, COALESCE(duration, 0) AS duration FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()?;
    let size: i64 = row.get("size");
    let duration: f64 = row.get("duration");
    let src_kbps = if duration > 0.0 {
        size as f64 * 8.0 / duration / 1000.0
    } else {
        0.0
    };
    // 源码率低于镜像目标：直传原文件（更小且无损）
    if src_kbps < cfg.mirror_bitrate as f64 {
        return None;
    }
    // 客户端上限不低于源码率：直传无损
    if max_br > 0 && src_kbps <= max_br as f64 {
        return None;
    }
    let p = mirror_path(cfg, track_id);
    (p.is_file()).then_some(p)
}

/// 增量同步。dry_run=true 时只统计不动手（即 status）。
pub async fn sync(pool: &SqlitePool, cfg: &Config, force: bool, dry_run: bool) -> Result<MirrorStats> {
    if !cfg.mirror_enabled {
        bail!("mirror disabled (enable with --mirror or default)");
    }
    if !dry_run {
        std::fs::create_dir_all(&cfg.mirror_dir)?;
        ensure_ffmpeg(cfg).await?;
    }
    let mut stats = MirrorStats::default();

    let rows = sqlx::query(
        "SELECT t.id, t.folder, t.path, t.size, t.mtime, COALESCE(t.duration, 0) AS duration,
                m.track_id AS mirrored, m.src_size AS m_size, m.src_mtime AS m_mtime
         FROM tracks t LEFT JOIN mirror_files m ON m.track_id = t.id",
    )
    .fetch_all(pool)
    .await?;
    let orphans = sqlx::query(
        "SELECT m.track_id FROM mirror_files m LEFT JOIN tracks t ON t.id = m.track_id WHERE t.id IS NULL",
    )
    .fetch_all(pool)
    .await?;

    stats.total = rows.len();

    for r in &rows {
        let id: i64 = r.get("id");
        let size: i64 = r.get("size");
        let mtime: i64 = r.get("mtime");
        let duration: f64 = r.get("duration");
        let has_row = r.get::<Option<i64>, _>("mirrored").is_some();
        let src_kbps = if duration > 0.0 {
            size as f64 * 8.0 / duration / 1000.0
        } else {
            0.0
        };

        // 低码率源不值得镜像（转出来更大），已有镜像则清掉
        if src_kbps < cfg.mirror_bitrate as f64 {
            if has_row {
                if !dry_run {
                    remove_mirror(pool, cfg, id).await?;
                }
                stats.removed += 1;
            } else {
                stats.skipped_small += 1;
            }
            continue;
        }
        stats.eligible += 1;

        let fresh = has_row
            && r.try_get::<i64, _>("m_size").unwrap_or(0) == size
            && r.try_get::<i64, _>("m_mtime").unwrap_or(0) == mtime
            && mirror_path(cfg, id).is_file();
        if fresh && !force {
            stats.covered += 1;
            continue;
        }

        if dry_run {
            if has_row {
                stats.retranscoded += 1;
            } else {
                stats.transcoded += 1;
            }
            continue;
        }

        let path: String = r.get("path");
        let folder: i64 = r.get("folder");
        let Some(src) = source_path(cfg, folder, &path) else {
            warn!(track = id, "music folder missing for track");
            stats.failed += 1;
            continue;
        };
        let dst = mirror_path(cfg, id);
        match transcode_one(cfg, &src, &dst).await {
            Ok(()) => {
                sqlx::query(
                    "INSERT INTO mirror_files(track_id, src_size, src_mtime, created)
                     VALUES(?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                     ON CONFLICT(track_id) DO UPDATE SET
                       src_size = excluded.src_size, src_mtime = excluded.src_mtime, created = excluded.created",
                )
                .bind(id)
                .bind(size)
                .bind(mtime)
                .execute(pool)
                .await?;
                if has_row {
                    stats.retranscoded += 1;
                } else {
                    stats.transcoded += 1;
                }
                stats.covered += 1;
            }
            Err(e) => {
                warn!(track = id, error = %e, "mirror transcode failed");
                stats.failed += 1;
            }
        }
        let done = stats.transcoded + stats.retranscoded;
        if done % 20 == 0 {
            info!("mirror sync: {done} transcoded");
        }
    }

    // 孤儿镜像：源曲目已删除，文件 + 记录一起清
    for r in orphans {
        let id: i64 = r.get("track_id");
        if !dry_run {
            remove_mirror(pool, cfg, id).await?;
        }
        stats.removed += 1;
    }

    Ok(stats)
}

async fn ensure_ffmpeg(cfg: &Config) -> Result<()> {
    match tokio::process::Command::new(&cfg.ffmpeg)
        .arg("-version")
        .output()
        .await
    {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => bail!("ffmpeg exited abnormally: {}", out.status),
        Err(e) => bail!("ffmpeg not found ({}): {e}", cfg.ffmpeg),
    }
}

/// 转码单首：写 .part 临时文件，成功后原子 rename，客户端永远见不到半个文件。
async fn transcode_one(cfg: &Config, src: &Path, dst: &Path) -> Result<()> {
    let mut tmp_name = dst.as_os_str().to_os_string();
    tmp_name.push(".part");
    let tmp = PathBuf::from(tmp_name);

    let out = tokio::process::Command::new(&cfg.ffmpeg)
        .args(["-y", "-nostdin"])
        .arg("-i")
        .arg(src)
        .args([
            "-vn",
            "-map_metadata",
            "0",
            "-id3v2_version",
            "3",
            "-codec:a",
            "libmp3lame",
            // 临时文件后缀是 .part，ffmpeg 推不出封装格式，需显式指定
            "-f",
            "mp3",
        ])
        .arg("-b:a")
        .arg(format!("{}k", cfg.mirror_bitrate))
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await?;

    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let tail = String::from_utf8_lossy(&out.stderr);
        let last = tail.lines().last().unwrap_or("");
        bail!("ffmpeg failed: {last}");
    }
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

async fn remove_mirror(pool: &SqlitePool, cfg: &Config, track_id: i64) -> Result<()> {
    let _ = tokio::fs::remove_file(mirror_path(cfg, track_id)).await;
    sqlx::query("DELETE FROM mirror_files WHERE track_id = ?")
        .bind(track_id)
        .execute(pool)
        .await?;
    Ok(())
}
