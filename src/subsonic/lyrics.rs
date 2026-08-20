use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use sqlx::Row;

use crate::auth::SubsonicAuth;
use crate::subsonic::response::{error, ok};
use crate::AppState;

fn structured_json(lyrics: &crate::lyrics::Lyrics, artist: &str, title: &str) -> Value {
    let lines: Vec<Value> = lyrics
        .lines
        .iter()
        .map(|l| match l.start_ms {
            Some(ms) => json!({ "start": ms, "value": l.text }),
            None => json!({ "value": l.text }),
        })
        .collect();
    json!({
        "displayArtist": artist,
        "displayTitle": title,
        "lang": "",
        "offset": lyrics.offset_ms,
        "synced": lyrics.synced,
        "line": lines,
    })
}

/// 歌词回退链：内嵌/.lrc（for_track）→ 磁盘缓存 → LRCLIB 在线查询（结果与未命中都缓存）。
async fn lyrics_with_fallback(
    st: &AppState,
    track_db_id: i64,
    path: &std::path::Path,
    artist: &str,
    title: &str,
    album: &str,
    duration_secs: i64,
) -> Option<crate::lyrics::Lyrics> {
    if let Some(l) = crate::lyrics::for_track(path).await {
        return Some(l);
    }

    let dir = st.cfg.data_dir.join("lyrics");
    let cache = dir.join(format!("{track_db_id}.lrc"));
    if let Ok(content) = tokio::fs::read_to_string(&cache).await {
        return if content.trim().is_empty() {
            None // 负缓存：之前查过没有
        } else {
            Some(crate::lyrics::from_text(&content))
        };
    }

    let fetched = crate::lyrics::fetch_lrclib(artist, title, album, duration_secs).await;
    let _ = tokio::fs::create_dir_all(&dir).await;
    let _ = tokio::fs::write(&cache, fetched.as_deref().unwrap_or("")).await;
    fetched.map(|text| crate::lyrics::from_text(&text))
}

/// OpenSubsonic 结构化歌词：/rest/getLyricsBySongId?id=tr-X
pub async fn get_lyrics_by_song_id(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = q.get("id") else {
        return error(10, "missing id");
    };
    let track_db_id = crate::subsonic::parse_track_id(id).unwrap_or(-1);

    let info = sqlx::query(
        "SELECT t.title, t.duration, ar.name AS artist_name, al.name AS album_name FROM tracks t
         LEFT JOIN artists ar ON ar.id = t.artist_id
         LEFT JOIN albums al ON al.id = t.album_id WHERE t.id = ?",
    )
    .bind(track_db_id)
    .fetch_optional(&st.pool)
    .await
    .ok()
    .flatten();

    let Some(path) = crate::cover::track_path_for(&st.pool, &st.cfg.music_dirs, id)
        .await
        .ok()
        .flatten()
    else {
        return error(70, "song not found");
    };

    let (artist, title, album, duration) = info
        .map(|r| {
            (
                r.try_get::<Option<String>, _>("artist_name").ok().flatten().unwrap_or_default(),
                r.get::<String, _>("title"),
                r.try_get::<Option<String>, _>("album_name").ok().flatten().unwrap_or_default(),
                r.get::<Option<f64>, _>("duration").unwrap_or(0.0) as i64,
            )
        })
        .unwrap_or_default();

    let lyrics =
        lyrics_with_fallback(&st, track_db_id, &path, &artist, &title, &album, duration).await;

    let structured: Vec<Value> = lyrics
        .as_ref()
        .map(|l| vec![structured_json(l, &artist, &title)])
        .unwrap_or_default();

    ok(json!({ "lyricsList": { "structuredLyrics": structured } })).into_response()
}

/// 老接口：/rest/getLyrics?artist=&title=（模糊匹配第一首）。
pub async fn get_lyrics(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let title = q.get("title").cloned().unwrap_or_default();
    let artist = q.get("artist").cloned().unwrap_or_default();

    let row = sqlx::query(
        "SELECT t.id, t.title, t.duration, ar.name AS artist_name, al.name AS album_name FROM tracks t
         LEFT JOIN artists ar ON ar.id = t.artist_id
         LEFT JOIN albums al ON al.id = t.album_id
         WHERE t.title LIKE ? AND (? = '' OR ar.name LIKE ?) LIMIT 1",
    )
    .bind(format!("%{title}%"))
    .bind(&artist)
    .bind(format!("%{artist}%"))
    .fetch_optional(&st.pool)
    .await
    .ok()
    .flatten();

    let Some(row) = row else {
        return ok(json!({ "lyrics": {} })).into_response();
    };

    let track_db_id = row.get::<i64, _>("id");
    let tid = crate::subsonic::track_id(track_db_id);
    let artist_name = row
        .try_get::<Option<String>, _>("artist_name")
        .ok()
        .flatten()
        .unwrap_or_default();
    let album_name = row
        .try_get::<Option<String>, _>("album_name")
        .ok()
        .flatten()
        .unwrap_or_default();
    let track_title: String = row.get("title");
    let duration = row.get::<Option<f64>, _>("duration").unwrap_or(0.0) as i64;

    let lyrics = match crate::cover::track_path_for(&st.pool, &st.cfg.music_dirs, &tid).await {
        Ok(Some(p)) => {
            lyrics_with_fallback(&st, track_db_id, &p, &artist_name, &track_title, &album_name, duration).await
        }
        _ => None,
    };

    match lyrics {
        Some(l) => ok(json!({
            "lyrics": {
                "artist": artist_name,
                "title": track_title,
                "value": l.raw,
            }
        }))
        .into_response(),
        None => ok(json!({ "lyrics": {} })).into_response(),
    }
}
