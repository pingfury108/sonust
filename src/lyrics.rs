use std::path::Path;

use lofty::file::TaggedFileExt;
use lofty::probe::Probe;

#[derive(Debug)]
pub struct LyricLine {
    pub start_ms: Option<u64>,
    pub text: String,
}

#[derive(Debug)]
pub struct Lyrics {
    pub raw: String,
    pub synced: bool,
    pub offset_ms: i64,
    pub lines: Vec<LyricLine>,
}

/// "mm:ss.xx" / "mm:ss.xxx" -> 毫秒
fn parse_timestamp(tag: &str) -> Option<u64> {
    let (min, sec) = tag.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    let (s, frac) = sec.split_once('.').unwrap_or((sec, "0"));
    let s: u64 = s.trim().parse().ok()?;
    let frac_ms: u64 = match frac.len() {
        1 => frac.parse::<u64>().ok()? * 100,
        2 => frac.parse::<u64>().ok()? * 10,
        _ => frac[..3].parse().ok()?,
    };
    Some((min * 60 + s) * 1000 + frac_ms)
}

/// 解析 LRC 文本：提取时间轴行，忽略 ti/ar/al 等元数据标签。
pub fn parse_lrc(content: &str) -> Lyrics {
    let mut lines = Vec::new();
    let mut offset_ms = 0i64;
    for raw_line in content.lines() {
        let mut rest = raw_line.trim();
        let mut stamps = Vec::new();
        let mut is_meta = false;
        while let Some(tag) = rest.strip_prefix('[').and_then(|r| r.find(']').map(|e| &r[..e])) {
            if let Some(ms) = parse_timestamp(tag) {
                stamps.push(ms);
                rest = rest[tag.len() + 2..].trim_start();
            } else {
                if let Some(v) = tag.strip_prefix("offset:") {
                    offset_ms = v.trim().parse().unwrap_or(0);
                }
                is_meta = stamps.is_empty();
                break;
            }
        }
        if is_meta || stamps.is_empty() {
            continue;
        }
        for ms in stamps {
            lines.push(LyricLine {
                start_ms: Some(ms),
                text: rest.to_string(),
            });
        }
    }
    lines.sort_by_key(|l| l.start_ms);
    Lyrics {
        raw: content.to_string(),
        synced: !lines.is_empty(),
        offset_ms,
        lines,
    }
}

/// 无时间轴的纯文本歌词。
fn from_plain_text(text: &str) -> Lyrics {
    Lyrics {
        raw: text.to_string(),
        synced: false,
        offset_ms: 0,
        lines: text
            .lines()
            .map(|l| LyricLine {
                start_ms: None,
                text: l.to_string(),
            })
            .collect(),
    }
}

/// 文本（LRC 或纯文本）→ Lyrics。
pub fn from_text(text: &str) -> Lyrics {
    let parsed = parse_lrc(text);
    if parsed.synced {
        parsed
    } else {
        from_plain_text(text)
    }
}

/// 查询 LRCLIB 在线歌词库（curl 子进程，避免引入 TLS 依赖破坏 musl 静态编译）。
/// 返回 syncedLyrics 或 plainLyrics 原文。
pub async fn fetch_lrclib(artist: &str, title: &str, album: &str, duration_secs: i64) -> Option<String> {
    let enc = |s: &str| form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    let url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}&album_name={}&duration={}",
        enc(artist),
        enc(title),
        enc(album),
        duration_secs
    );
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        tokio::process::Command::new("curl")
            .args(["-sS", "--max-time", "5", &url])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v.get("syncedLyrics")
        .or_else(|| v.get("plainLyrics"))?
        .as_str()
        .map(String::from)
        .filter(|s| !s.trim().is_empty())
}

/// 获取曲目歌词：内嵌标签优先，同目录同名 .lrc 兜底。
pub async fn for_track(track_path: &Path) -> Option<Lyrics> {
    let path = track_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        // 1. 内嵌歌词（ID3 USLT / Vorbis LYRICS）
        if let Ok(tagged) = Probe::open(&path).and_then(|p| p.read()) {
            if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
                if let Some(text) = tag.get_string(&lofty::tag::ItemKey::Lyrics) {
                    if !text.trim().is_empty() {
                        return Some(from_text(text));
                    }
                }
            }
        }
        // 2. 同名 .lrc 边车文件
        let lrc = path.with_extension("lrc");
        if let Ok(content) = std::fs::read_to_string(&lrc) {
            if !content.trim().is_empty() {
                return Some(parse_lrc(&content));
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}
