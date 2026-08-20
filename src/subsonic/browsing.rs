use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Json, Response},
};
use serde_json::{json, Value};
use sqlx::Row;

use crate::auth::SubsonicAuth;
use crate::subsonic::response::{error, ok};
use crate::subsonic::{album_id, artist_id, parse_album_id, parse_artist_id, track_id};
use crate::AppState;

/// 带 join 的曲目查询基础 SQL。
pub const SONG_SQL: &str = r#"
SELECT t.id, t.title, t.track_no, t.year, t.genre, t.duration, t.format, t.size, t.path,
       t.has_cover, t.album_id, t.artist_id,
       al.name AS album_name, ar.name AS artist_name,
       strftime('%Y-%m-%dT%H:%M:%SZ', t.mtime, 'unixepoch') AS created,
       (SELECT s.created FROM starred s WHERE s.item_type = 'track' AND s.item_id = t.id) AS starred
FROM tracks t
LEFT JOIN albums al ON al.id = t.album_id
LEFT JOIN artists ar ON ar.id = t.artist_id
"#;

/// Subsonic Child（song）JSON。
pub fn song_json(row: &sqlx::sqlite::SqliteRow) -> Value {
    let id: i64 = row.get("id");
    let format: Option<String> = row.get("format");
    let has_cover: bool = row.get("has_cover");
    let album_id_v: Option<i64> = row.get("album_id");
    let artist_id_v: Option<i64> = row.get("artist_id");
    let suffix = format.clone().unwrap_or_default();

    let mut song = json!({
        "id": track_id(id),
        "isDir": false,
        "title": row.get::<String, _>("title"),
        "track": row.get::<Option<i64>, _>("track_no"),
        "year": row.get::<Option<i64>, _>("year"),
        "genre": row.get::<Option<String>, _>("genre"),
        "duration": row.get::<Option<f64>, _>("duration").map(|d| d as i64),
        "size": row.get::<i64, _>("size"),
        "suffix": suffix,
        "contentType": content_type(&suffix),
        "path": row.get::<String, _>("path"),
        "type": "music",
        "mediaType": "music",
        "created": row.get::<Option<String>, _>("created"),
    });
    let m = song.as_object_mut().unwrap();
    if let (Some(aid), Some(name)) = (album_id_v, row.get::<Option<String>, _>("album_name")) {
        m.insert("albumId".into(), json!(album_id(aid)));
        m.insert("album".into(), json!(name));
        m.insert("parent".into(), json!(album_id(aid)));
    }
    if let (Some(aid), Some(name)) = (artist_id_v, row.get::<Option<String>, _>("artist_name")) {
        m.insert("artistId".into(), json!(artist_id(aid)));
        m.insert("artist".into(), json!(name));
    }
    if has_cover {
        m.insert("coverArt".into(), json!(track_id(id)));
    }
    if let Ok(Some(starred)) = row.try_get::<Option<String>, _>("starred") {
        m.insert("starred".into(), json!(starred));
    }
    song
}

pub fn content_type(suffix: &str) -> &'static str {
    match suffix {
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "m4a" | "aac" => "audio/mp4",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "wma" => "audio/x-ms-wma",
        "aiff" => "audio/aiff",
        _ => "application/octet-stream",
    }
}

/// 专辑查询附带的 created / 封面统计子查询片段。
pub const ALBUM_EXTRA_SQL: &str = "
    (SELECT strftime('%Y-%m-%dT%H:%M:%SZ', MIN(t.mtime), 'unixepoch') FROM tracks t WHERE t.album_id = al.id) AS created,
    (SELECT COUNT(*) FROM tracks t WHERE t.album_id = al.id AND t.has_cover = 1) AS cover_count";

/// Subsonic AlbumID3 JSON。
pub fn album_json(row: &sqlx::sqlite::SqliteRow, song_count: i64, duration: i64) -> Value {
    let id: i64 = row.get("id");
    let artist_id_v: Option<i64> = row.get("artist_id");
    let mut album = json!({
        "id": album_id(id),
        "name": row.get::<String, _>("name"),
        "songCount": song_count,
        "duration": duration,
        "year": row.get::<Option<i64>, _>("year"),
        "created": row
            .try_get::<Option<String>, _>("created")
            .ok()
            .flatten()
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".into()),
    });
    if row.try_get::<i64, _>("cover_count").unwrap_or(0) > 0 {
        album.as_object_mut().unwrap().insert("coverArt".into(), json!(album_id(id)));
    }
    if let Some(aid) = artist_id_v {
        let m = album.as_object_mut().unwrap();
        m.insert("artistId".into(), json!(artist_id(aid)));
        if let Ok(Some(name)) = row.try_get::<Option<String>, _>("artist_name") {
            m.insert("artist".into(), json!(name));
        }
    }
    if let Ok(Some(starred)) = row.try_get::<Option<String>, _>("starred") {
        album.as_object_mut().unwrap().insert("starred".into(), json!(starred));
    }
    album
}

pub async fn get_music_folders(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
) -> Json<Value> {
    let folders: Vec<Value> = st
        .cfg
        .music_dirs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            json!({
                "id": i as i64,
                "name": d.file_name().map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| d.display().to_string()),
            })
        })
        .collect();
    ok(json!({ "musicFolders": { "musicFolder": folders } }))
}

pub async fn get_artists(State(st): State<AppState>, _auth: SubsonicAuth) -> Response {
    let rows = match sqlx::query(
        "SELECT ar.id, ar.name, COUNT(al.id) AS album_count
         FROM artists ar
         LEFT JOIN albums al ON al.artist_id = ar.id
         GROUP BY ar.id
         ORDER BY ar.name COLLATE NOCASE",
    )
    .fetch_all(&st.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return error(0, &e.to_string()),
    };

    // 按首字母分组为 index
    let mut indexes: HashMap<String, Vec<Value>> = HashMap::new();
    for r in &rows {
        let name: String = r.get("name");
        let key = name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .filter(|s| s.chars().next().is_some_and(|c| c.is_ascii_alphabetic()))
            .unwrap_or_else(|| "#".into());
        indexes.entry(key).or_default().push(json!({
            "id": artist_id(r.get::<i64, _>("id")),
            "name": name,
            "albumCount": r.get::<i64, _>("album_count"),
            "coverArt": artist_id(r.get::<i64, _>("id")),
        }));
    }
    let mut index_list: Vec<Value> = indexes
        .into_iter()
        .map(|(name, artist)| json!({ "name": name, "artist": artist }))
        .collect();
    index_list.sort_by_key(|v| v["name"].as_str().map(String::from).unwrap_or_default());

    ok(json!({
        "artists": {
            "ignoredArticles": "The El La Los Las Le Les",
            "index": index_list
        }
    }))
    .into_response()
}

pub async fn get_artist(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = q.get("id").and_then(|s| parse_artist_id(s)) else {
        return error(10, "missing or invalid id");
    };

    let artist = match sqlx::query("SELECT id, name FROM artists WHERE id = ?")
        .bind(id)
        .fetch_optional(&st.pool)
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return error(70, "artist not found"),
        Err(e) => return error(0, &e.to_string()),
    };

    let albums = sqlx::query(&format!(
        "SELECT al.*, ar.name AS artist_name,
                (SELECT COUNT(*) FROM tracks t WHERE t.album_id = al.id) AS song_count,
                (SELECT COALESCE(SUM(t.duration), 0) FROM tracks t WHERE t.album_id = al.id) AS duration,
                (SELECT s.created FROM starred s WHERE s.item_type = 'album' AND s.item_id = al.id) AS starred,
                {ALBUM_EXTRA_SQL}
         FROM albums al LEFT JOIN artists ar ON ar.id = al.artist_id
         WHERE al.artist_id = ? ORDER BY al.year, al.name"
    ))
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    let album_list: Vec<Value> = albums
        .iter()
        .map(|r| album_json(r, r.get("song_count"), r.get::<f64, _>("duration") as i64))
        .collect();
    let album_count = album_list.len() as i64;

    ok(json!({
        "artist": {
            "id": artist_id(id),
            "name": artist.get::<String, _>("name"),
            "albumCount": album_count,
            "coverArt": artist_id(id),
            "album": album_list,
        }
    }))
    .into_response()
}

pub async fn get_album(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = q.get("id").and_then(|s| parse_album_id(s)) else {
        return error(10, "missing or invalid id");
    };

    let album = match sqlx::query(&format!(
        "SELECT al.*, ar.name AS artist_name,
                (SELECT s.created FROM starred s WHERE s.item_type = 'album' AND s.item_id = al.id) AS starred,
                {ALBUM_EXTRA_SQL}
         FROM albums al
         LEFT JOIN artists ar ON ar.id = al.artist_id WHERE al.id = ?"
    ))
    .bind(id)
    .fetch_optional(&st.pool)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => return error(70, "album not found"),
        Err(e) => return error(0, &e.to_string()),
    };

    let songs = sqlx::query(&format!("{SONG_SQL} WHERE t.album_id = ? ORDER BY t.track_no, t.title"))
        .bind(id)
        .fetch_all(&st.pool)
        .await
        .unwrap_or_default();

    let song_list: Vec<Value> = songs.iter().map(song_json).collect();
    let duration: i64 = songs
        .iter()
        .map(|r| r.get::<Option<f64>, _>("duration").unwrap_or(0.0) as i64)
        .sum();

    let mut a = album_json(&album, song_list.len() as i64, duration);
    a.as_object_mut()
        .unwrap()
        .insert("song".into(), json!(song_list));

    ok(json!({ "album": a })).into_response()
}
