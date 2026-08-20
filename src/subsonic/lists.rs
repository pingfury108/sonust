use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use sqlx::Row;

use crate::auth::SubsonicAuth;
use crate::subsonic::browsing::{album_json, song_json, ALBUM_EXTRA_SQL, SONG_SQL};
use crate::subsonic::response::{error, ok};
use crate::AppState;

fn i64_param(q: &HashMap<String, String>, key: &str, default: i64) -> i64 {
    q.get(key).and_then(|s| s.parse().ok()).unwrap_or(default)
}

pub async fn get_genres(State(st): State<AppState>, _auth: SubsonicAuth) -> Response {
    let rows = sqlx::query(
        "SELECT genre, COUNT(*) AS song_count, COUNT(DISTINCT album_id) AS album_count
         FROM tracks WHERE genre IS NOT NULL AND genre != ''
         GROUP BY genre ORDER BY genre COLLATE NOCASE",
    )
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    let genres: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "value": r.get::<String, _>("genre"),
                "songCount": r.get::<i64, _>("song_count"),
                "albumCount": r.get::<i64, _>("album_count"),
            })
        })
        .collect();

    ok(json!({ "genres": { "genre": genres } })).into_response()
}

pub async fn get_album_list2(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let size = i64_param(&q, "size", 10).clamp(1, 500);
    let offset = i64_param(&q, "offset", 0).max(0);
    let list_type = q.get("type").map(String::as_str).unwrap_or("random");

    let (order, cond) = match list_type {
        "newest" => ("al.id DESC", ""),
        "alphabeticalByName" => ("al.name COLLATE NOCASE", ""),
        "alphabeticalByArtist" => ("ar.name COLLATE NOCASE, al.name COLLATE NOCASE", ""),
        "starred" => ("starred DESC", "WHERE starred IS NOT NULL"),
        "recent" | "frequent" | "highest" => ("al.id DESC", ""), // MVP：无播放统计，退化为最新
        _ => ("RANDOM()", ""), // random 及未知类型
    };

    let rows = match sqlx::query(&format!(
        "SELECT al.*, ar.name AS artist_name,
                (SELECT COUNT(*) FROM tracks t WHERE t.album_id = al.id) AS song_count,
                (SELECT COALESCE(SUM(t.duration), 0) FROM tracks t WHERE t.album_id = al.id) AS duration,
                (SELECT s.created FROM starred s WHERE s.item_type = 'album' AND s.item_id = al.id) AS starred,
                {ALBUM_EXTRA_SQL}
         FROM albums al LEFT JOIN artists ar ON ar.id = al.artist_id
         {cond} ORDER BY {order} LIMIT ? OFFSET ?"
    ))
    .bind(size)
    .bind(offset)
    .fetch_all(&st.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return error(0, &e.to_string()),
    };

    let albums: Vec<Value> = rows
        .iter()
        .map(|r| album_json(r, r.get("song_count"), r.get::<f64, _>("duration") as i64))
        .collect();

    ok(json!({ "albumList2": { "album": albums } })).into_response()
}

pub async fn get_random_songs(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let size = i64_param(&q, "size", 10).clamp(1, 500);

    let mut sql = format!("{SONG_SQL} WHERE 1=1");
    if let Some(genre) = q.get("genre").filter(|g| !g.is_empty()) {
        sql.push_str(&format!(" AND t.genre = '{}'", genre.replace('\'', "''")));
    }
    sql.push_str(" ORDER BY RANDOM() LIMIT ?");

    let rows = match sqlx::query(&sql).bind(size).fetch_all(&st.pool).await {
        Ok(r) => r,
        Err(e) => return error(0, &e.to_string()),
    };

    let songs: Vec<Value> = rows.iter().map(song_json).collect();
    ok(json!({ "randomSongs": { "song": songs } })).into_response()
}

pub async fn get_songs_by_genre(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(genre) = q.get("genre").filter(|g| !g.is_empty()) else {
        return error(10, "missing genre");
    };
    let count = i64_param(&q, "count", 10).clamp(1, 500);
    let offset = i64_param(&q, "offset", 0).max(0);

    let rows = match sqlx::query(&format!(
        "{SONG_SQL} WHERE t.genre = ? ORDER BY ar.name COLLATE NOCASE, al.name, t.track_no LIMIT ? OFFSET ?"
    ))
    .bind(genre)
    .bind(count)
    .bind(offset)
    .fetch_all(&st.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return error(0, &e.to_string()),
    };

    let songs: Vec<Value> = rows.iter().map(song_json).collect();
    ok(json!({ "songsByGenre": { "song": songs } })).into_response()
}

// 以下为 Symfonium 同步会探测的端点，数据不足时返回空结果（协议合法）。

pub async fn get_top_songs(_auth: SubsonicAuth) -> Response {
    ok(json!({ "topSongs": { "song": [] } })).into_response()
}

pub async fn get_similar_songs2(_auth: SubsonicAuth) -> Response {
    ok(json!({ "similarSongs2": { "song": [] } })).into_response()
}

pub async fn get_artist_info2(_auth: SubsonicAuth) -> Response {
    ok(json!({ "artistInfo2": {} })).into_response()
}

pub async fn get_playlists(_auth: SubsonicAuth) -> Response {
    ok(json!({ "playlists": { "playlist": [] } })).into_response()
}

pub async fn get_bookmarks(_auth: SubsonicAuth) -> Response {
    ok(json!({ "bookmarks": { "bookmark": [] } })).into_response()
}

/// 书签写入暂无持久化，静默成功避免客户端同步报错。
pub async fn create_bookmark(_auth: SubsonicAuth) -> Response {
    ok(json!({})).into_response()
}

pub async fn delete_bookmark(_auth: SubsonicAuth) -> Response {
    ok(json!({})).into_response()
}

pub async fn get_play_queue(_auth: SubsonicAuth) -> Response {
    ok(json!({ "playQueue": {} })).into_response()
}

/// 播放队列保存暂无持久化，静默成功。
pub async fn save_play_queue(_auth: SubsonicAuth) -> Response {
    ok(json!({})).into_response()
}

pub async fn search3(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let query = q.get("query").cloned().unwrap_or_default();
    let query = query.trim_matches('"');
    let artist_count = i64_param(&q, "artistCount", 20).clamp(0, 500);
    let album_count = i64_param(&q, "albumCount", 20).clamp(0, 500);
    let song_count = i64_param(&q, "songCount", 20).clamp(0, 500);
    let artist_offset = i64_param(&q, "artistOffset", 0).max(0);
    let album_offset = i64_param(&q, "albumOffset", 0).max(0);
    let song_offset = i64_param(&q, "songOffset", 0).max(0);

    // 空查询 = 匹配全部（Navidrome 约定，Symfonium 全库同步依赖此行为）
    let like = if query.is_empty() {
        "%".to_string()
    } else {
        format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"))
    };

    let artists = sqlx::query(
        "SELECT ar.id, ar.name, COUNT(al.id) AS album_count FROM artists ar
         LEFT JOIN albums al ON al.artist_id = ar.id
         WHERE ar.name LIKE ? ESCAPE '\\' GROUP BY ar.id ORDER BY ar.name COLLATE NOCASE LIMIT ? OFFSET ?",
    )
    .bind(&like)
    .bind(artist_count)
    .bind(artist_offset)
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    let albums = sqlx::query(&format!(
        "SELECT al.*, ar.name AS artist_name,
                (SELECT COUNT(*) FROM tracks t WHERE t.album_id = al.id) AS song_count,
                (SELECT COALESCE(SUM(t.duration), 0) FROM tracks t WHERE t.album_id = al.id) AS duration,
                (SELECT s.created FROM starred s WHERE s.item_type = 'album' AND s.item_id = al.id) AS starred,
                {ALBUM_EXTRA_SQL}
         FROM albums al LEFT JOIN artists ar ON ar.id = al.artist_id
         WHERE al.name LIKE ? ESCAPE '\\' ORDER BY al.name COLLATE NOCASE LIMIT ? OFFSET ?"
    ))
    .bind(&like)
    .bind(album_count)
    .bind(album_offset)
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    let songs = sqlx::query(&format!(
        "{SONG_SQL} WHERE t.title LIKE ? ESCAPE '\\' ORDER BY t.title COLLATE NOCASE LIMIT ? OFFSET ?"
    ))
    .bind(&like)
    .bind(song_count)
    .bind(song_offset)
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    ok(json!({
        "searchResult3": {
            "artist": artists.iter().map(|r| json!({
                "id": crate::subsonic::artist_id(r.get::<i64, _>("id")),
                "name": r.get::<String, _>("name"),
                "albumCount": r.get::<i64, _>("album_count"),
            })).collect::<Vec<_>>(),
            "album": albums.iter().map(|r| album_json(r, r.get("song_count"), r.get::<f64, _>("duration") as i64)).collect::<Vec<_>>(),
            "song": songs.iter().map(song_json).collect::<Vec<_>>(),
        }
    }))
    .into_response()
}
