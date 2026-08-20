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

/// OpenSubsonic 结构化歌词：/rest/getLyricsBySongId?id=tr-X
pub async fn get_lyrics_by_song_id(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = q.get("id") else {
        return error(10, "missing id");
    };

    let info = sqlx::query(
        "SELECT t.title, ar.name AS artist_name FROM tracks t
         LEFT JOIN artists ar ON ar.id = t.artist_id WHERE t.id = ?",
    )
    .bind(crate::subsonic::parse_track_id(id).unwrap_or(-1))
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

    let lyrics = crate::lyrics::for_track(&path).await;
    let (artist, title) = info
        .map(|r| {
            (
                r.try_get::<Option<String>, _>("artist_name").ok().flatten().unwrap_or_default(),
                r.get::<String, _>("title"),
            )
        })
        .unwrap_or_default();

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
        "SELECT t.id, t.title, ar.name AS artist_name FROM tracks t
         LEFT JOIN artists ar ON ar.id = t.artist_id
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

    let tid = crate::subsonic::track_id(row.get::<i64, _>("id"));
    let lyrics = match crate::cover::track_path_for(&st.pool, &st.cfg.music_dirs, &tid).await {
        Ok(Some(p)) => crate::lyrics::for_track(&p).await,
        _ => None,
    };

    match lyrics {
        Some(l) => ok(json!({
            "lyrics": {
                "artist": row.try_get::<Option<String>, _>("artist_name").ok().flatten(),
                "title": row.get::<String, _>("title"),
                "value": l.raw,
            }
        }))
        .into_response(),
        None => ok(json!({ "lyrics": {} })).into_response(),
    }
}
