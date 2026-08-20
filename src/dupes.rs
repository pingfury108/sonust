use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};

const LOSSLESS: &[&str] = &["flac", "wav", "aiff", "aif", "ape"];
/// 时长差在此范围内视为同一首歌（防止误并 Live 版/剪辑版）
const DURATION_TOLERANCE: f64 = 2.0;

#[derive(Debug, Clone)]
struct Track {
    id: i64,
    folder: i64,
    path: String,
    title: String,
    artist: String,
    format: String,
    size: i64,
    duration: f64,
    sha256: Option<String>,
}

pub struct Group {
    pub keep: Track,
    pub dupes: Vec<(Track, &'static str)>, // (副本, 判定原因)
}

/// 质量评分：无损优先，同格式取大文件，平分取先入的（id 小）。
fn score(t: &Track) -> (i64, i64, i64) {
    let lossless = i64::from(LOSSLESS.contains(&t.format.as_str()));
    (lossless, t.size, -t.id)
}

fn file_sha256(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).ok()?;
    Some(hex::encode(h.finalize()))
}

pub async fn find_groups(pool: &SqlitePool, music_dirs: &[PathBuf]) -> Result<Vec<Group>> {
    let rows = sqlx::query(
        "SELECT t.id, t.folder, t.path, t.title, t.format, t.size, t.duration,
                COALESCE(ar.name, '') AS artist
         FROM tracks t LEFT JOIN artists ar ON ar.id = t.artist_id",
    )
    .fetch_all(pool)
    .await?;

    let mut tracks: Vec<Track> = rows
        .iter()
        .map(|r| Track {
            id: r.get("id"),
            folder: r.get("folder"),
            path: r.get("path"),
            title: r.get::<String, _>("title").to_lowercase(),
            artist: r.get::<String, _>("artist").to_lowercase(),
            format: r.get::<Option<String>, _>("format").unwrap_or_default(),
            size: r.get("size"),
            duration: r.get::<Option<f64>, _>("duration").unwrap_or(0.0),
            sha256: None,
        })
        .collect();

    // 只对"同 (artist,title) 且同 size"的文件算哈希，避免全库 IO
    let mut size_count: HashMap<(String, String, i64), usize> = HashMap::new();
    for t in &tracks {
        *size_count
            .entry((t.artist.clone(), t.title.clone(), t.size))
            .or_default() += 1;
    }
    let need_hash: Vec<usize> = tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| size_count[&(t.artist.clone(), t.title.clone(), t.size)] > 1)
        .map(|(i, _)| i)
        .collect();
    if !need_hash.is_empty() {
        let dirs = music_dirs.to_vec();
        let jobs: Vec<(usize, PathBuf)> = need_hash
            .into_iter()
            .map(|i| (i, dirs[tracks[i].folder as usize].join(&tracks[i].path)))
            .collect();
        let hashes = tokio::task::spawn_blocking(move || {
            jobs.into_iter()
                .map(|(i, p)| (i, file_sha256(&p)))
                .collect::<Vec<_>>()
        })
        .await?;
        for (i, h) in hashes {
            tracks[i].sha256 = h;
        }
    }

    // (artist, title) 分组 -> 时长聚类
    let mut by_key: HashMap<(String, String), Vec<Track>> = HashMap::new();
    for t in tracks {
        by_key
            .entry((t.artist.clone(), t.title.clone()))
            .or_default()
            .push(t);
    }

    let mut groups = Vec::new();
    for (_, mut list) in by_key {
        if list.len() < 2 {
            continue;
        }
        list.sort_by(|a, b| a.duration.partial_cmp(&b.duration).unwrap());
        let mut clusters: Vec<Vec<Track>> = Vec::new();
        for t in list {
            match clusters.last_mut() {
                Some(last) if (t.duration - last[0].duration).abs() <= DURATION_TOLERANCE => {
                    last.push(t)
                }
                _ => clusters.push(vec![t]),
            }
        }
        for cluster in clusters {
            if cluster.len() < 2 {
                continue;
            }
            let mut sorted = cluster;
            sorted.sort_by_key(score);
            let keep = sorted.pop().expect("cluster non-empty");
            let dupes = sorted
                .into_iter()
                .rev()
                .map(|t| {
                    let reason = match (&t.sha256, &keep.sha256) {
                        (Some(a), Some(b)) if a == b => "字节相同",
                        _ => "同曲低质",
                    };
                    (t, reason)
                })
                .collect();
            groups.push(Group { keep, dupes });
        }
    }
    groups.sort_by_key(|g| -(g.dupes.len() as i64));
    Ok(groups)
}

pub fn print_report(groups: &[Group]) {
    let mut total_dupes = 0usize;
    let mut total_bytes = 0i64;
    for g in groups {
        let k = &g.keep;
        println!(
            "\n{} - {}（{} 份，时长 {}s）",
            k.artist,
            k.title,
            g.dupes.len() + 1,
            k.duration as i64
        );
        println!(
            "  KEEP  {:5} {:>5}MB  {}",
            k.format,
            k.size / 1048576,
            k.path
        );
        for (t, reason) in &g.dupes {
            println!(
                "  DUPE  {:5} {:>5}MB  {}  [{}]",
                t.format,
                t.size / 1048576,
                t.path,
                reason
            );
            total_dupes += 1;
            total_bytes += t.size;
        }
    }
    println!(
        "\n汇总: {} 组重复，{} 个可清理副本，约释放 {} MB",
        groups.len(),
        total_dupes,
        total_bytes / 1048576
    );
}

/// 把非保留副本移动到隔离目录（保留相对路径结构），同名冲突自动加序号。
/// 同目录下的 .lrc 一并移动。返回 (移动文件数, 字节数)。
pub async fn quarantine(
    groups: &[Group],
    music_dirs: &[PathBuf],
    quarantine_dir: &Path,
) -> Result<(usize, i64)> {
    let mut moved = 0usize;
    let mut bytes = 0i64;

    for g in groups {
        for (t, _) in &g.dupes {
            let src = music_dirs[t.folder as usize].join(&t.path);
            let dest = unique_dest(quarantine_dir.join(&t.path));
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            move_file(&src, &dest).await?;
            moved += 1;
            bytes += t.size;

            // 歌词边车一起走
            let lrc_src = src.with_extension("lrc");
            if lrc_src.exists() {
                let lrc_dest = dest.with_extension("lrc");
                let _ = move_file(&lrc_src, &lrc_dest).await;
            }
        }
    }
    Ok((moved, bytes))
}

fn unique_dest(dest: PathBuf) -> PathBuf {
    if !dest.exists() {
        return dest;
    }
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = dest
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    let parent = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    for n in 2..100 {
        let candidate = parent.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dest
}

/// rename 优先，跨设备回退 copy+delete。
async fn move_file(src: &Path, dest: &Path) -> Result<()> {
    match tokio::fs::rename(src, dest).await {
        Ok(()) => Ok(()),
        Err(_) => {
            tokio::fs::copy(src, dest).await?;
            tokio::fs::remove_file(src).await?;
            Ok(())
        }
    }
}
