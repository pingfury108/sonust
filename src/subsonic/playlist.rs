use std::collections::HashMap;

use axum::{
    extract::{Query, RawQuery, State},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};

use crate::auth::SubsonicAuth;
use crate::subsonic::browsing::{song_json, SONG_SQL};
use crate::subsonic::response::{error, ok};
use crate::subsonic::star::parse_multi;
use crate::AppState;

/// 播放列表 ID 约定：pl-{id}
fn pl_id(id: i64) -> String {
    format!("pl-{id}")
}
fn parse_pl_id(id: &str) -> Option<i64> {
    id.strip_prefix("pl-")
        .and_then(|s| s.parse().ok())
        .or_else(|| id.parse().ok())
}

fn playlist_json(row: &sqlx::sqlite::SqliteRow, song_count: i64, duration: i64) -> Value {
    json!({
        "id": pl_id(row.get("id")),
        "name": row.get::<String, _>("name"),
        "comment": row.try_get::<Option<String>, _>("comment").ok().flatten(),
        "owner": row.try_get::<Option<String>, _>("owner").ok().flatten(),
        "public": row.get::<i64, _>("public") != 0,
        "songCount": song_count,
        "duration": duration,
        "created": row.get::<String, _>("created"),
        "changed": row.get::<String, _>("changed"),
    })
}

const PL_SELECT: &str = "
    SELECT pl.*,
           (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id = pl.id) AS song_count,
           (SELECT COALESCE(SUM(t.duration), 0.0) FROM playlist_items i
            JOIN tracks t ON t.id = i.track_id WHERE i.playlist_id = pl.id) AS duration
    FROM playlists pl";

async fn load_playlist(pool: &SqlitePool, id: i64) -> Result<Option<Value>, sqlx::Error> {
    let Some(row) = sqlx::query(&format!("{PL_SELECT} WHERE pl.id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?
    else {
        return Ok(None);
    };

    let entries = sqlx::query(&format!(
        "{SONG_SQL} JOIN playlist_items i ON i.track_id = t.id
         WHERE i.playlist_id = ? ORDER BY i.position"
    ))
    .bind(id)
    .fetch_all(pool)
    .await?;

    let mut pl = playlist_json(&row, row.get("song_count"), row.get::<f64, _>("duration") as i64);
    pl.as_object_mut().unwrap().insert(
        "entry".into(),
        json!(entries.iter().map(song_json).collect::<Vec<_>>()),
    );
    Ok(Some(pl))
}

pub async fn get_playlists(State(st): State<AppState>, _auth: SubsonicAuth) -> Response {
    let rows = match sqlx::query(&format!("{PL_SELECT} ORDER BY pl.changed DESC"))
        .fetch_all(&st.pool)
        .await
    {
        Ok(r) => r,
        Err(e) => return error(0, &e.to_string()),
    };
    let list: Vec<Value> = rows
        .iter()
        .map(|r| playlist_json(r, r.get("song_count"), r.get::<f64, _>("duration") as i64))
        .collect();
    ok(json!({ "playlists": { "playlist": list } })).into_response()
}

pub async fn get_playlist(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = q.get("id").and_then(|s| parse_pl_id(s)) else {
        return error(10, "missing or invalid id");
    };
    match load_playlist(&st.pool, id).await {
        Ok(Some(pl)) => ok(json!({ "playlist": pl })).into_response(),
        Ok(None) => error(70, "playlist not found"),
        Err(e) => error(0, &e.to_string()),
    }
}

async fn touch(pool: &SqlitePool, id: i64) {
    let _ = sqlx::query(
        "UPDATE playlists SET changed = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?",
    )
    .bind(id)
    .execute(pool)
    .await;
}

async fn next_position(pool: &SqlitePool, playlist_id: i64) -> i64 {
    sqlx::query("SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_items WHERE playlist_id = ?")
        .bind(playlist_id)
        .fetch_one(pool)
        .await
        .map(|r| r.get(0))
        .unwrap_or(0)
}

/// createPlaylist：带 name 新建（Subsonic 1.14+），带 playlistId 则覆盖式更新（老行为）。
pub async fn create_playlist(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    RawQuery(raw): RawQuery,
) -> Response {
    let q = parse_multi(raw.as_deref().unwrap_or(""));
    let song_ids: Vec<i64> = q
        .get("songId")
        .into_iter()
        .flatten()
        .filter_map(|s| crate::subsonic::parse_track_id(s))
        .collect();

    // 老行为：createPlaylist?playlistId= 覆盖整个歌单
    if let Some(pid) = q.get("playlistId").and_then(|v| v.first()).and_then(|s| parse_pl_id(s)) {
        if let Err(e) = sqlx::query("DELETE FROM playlist_items WHERE playlist_id = ?")
            .bind(pid)
            .execute(&st.pool)
            .await
        {
            return error(0, &e.to_string());
        }
        for (pos, tid) in song_ids.iter().enumerate() {
            let _ = sqlx::query(
                "INSERT INTO playlist_items(playlist_id, track_id, position) VALUES(?, ?, ?)",
            )
            .bind(pid)
            .bind(tid)
            .bind(pos as i64)
            .execute(&st.pool)
            .await;
        }
        touch(&st.pool, pid).await;
        return match load_playlist(&st.pool, pid).await {
            Ok(Some(pl)) => ok(json!({ "playlist": pl })).into_response(),
            _ => error(70, "playlist not found"),
        };
    }

    let Some(name) = q.get("name").and_then(|v| v.first()).filter(|s| !s.is_empty()) else {
        return error(10, "missing name");
    };

    let res = sqlx::query(
        "INSERT INTO playlists(name, owner, created, changed)
         VALUES(?, ?, strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
    )
    .bind(name)
    .bind(&st.cfg.user)
    .execute(&st.pool)
    .await;
    let pid = match res {
        Ok(r) => r.last_insert_rowid(),
        Err(e) => return error(0, &e.to_string()),
    };
    for (pos, tid) in song_ids.iter().enumerate() {
        let _ = sqlx::query(
            "INSERT INTO playlist_items(playlist_id, track_id, position) VALUES(?, ?, ?)",
        )
        .bind(pid)
        .bind(tid)
        .bind(pos as i64)
        .execute(&st.pool)
        .await;
    }
    match load_playlist(&st.pool, pid).await {
        Ok(Some(pl)) => ok(json!({ "playlist": pl })).into_response(),
        _ => error(0, "failed to load created playlist"),
    }
}

pub async fn update_playlist(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    RawQuery(raw): RawQuery,
) -> Response {
    let q = parse_multi(raw.as_deref().unwrap_or(""));
    let Some(pid) = q.get("playlistId").and_then(|v| v.first()).and_then(|s| parse_pl_id(s)) else {
        return error(10, "missing playlistId");
    };

    if let Some(name) = q.get("name").and_then(|v| v.first()) {
        let _ = sqlx::query("UPDATE playlists SET name = ? WHERE id = ?")
            .bind(name)
            .bind(pid)
            .execute(&st.pool)
            .await;
    }
    if let Some(comment) = q.get("comment").and_then(|v| v.first()) {
        let _ = sqlx::query("UPDATE playlists SET comment = ? WHERE id = ?")
            .bind(comment)
            .bind(pid)
            .execute(&st.pool)
            .await;
    }
    if let Some(public) = q.get("public").and_then(|v| v.first()) {
        let _ = sqlx::query("UPDATE playlists SET public = ? WHERE id = ?")
            .bind(public == "true")
            .bind(pid)
            .execute(&st.pool)
            .await;
    }

    // 先删后加：songIndexToRemove 按降序删除保持索引有效
    let mut to_remove: Vec<i64> = q
        .get("songIndexToRemove")
        .into_iter()
        .flatten()
        .filter_map(|s| s.parse().ok())
        .collect();
    to_remove.sort_unstable_by(|a, b| b.cmp(a));
    for idx in to_remove {
        let _ = sqlx::query(
            "DELETE FROM playlist_items WHERE playlist_id = ? AND position = ?",
        )
        .bind(pid)
        .bind(idx)
        .execute(&st.pool)
        .await;
    }
    // 删除后压紧 position
    let _ = sqlx::query(
        "UPDATE playlist_items SET position = seq.new_pos FROM (
            SELECT id, ROW_NUMBER() OVER (ORDER BY position) - 1 AS new_pos
            FROM playlist_items WHERE playlist_id = ?
         ) seq WHERE playlist_items.id = seq.id",
    )
    .bind(pid)
    .execute(&st.pool)
    .await;

    for raw in q.get("songIdToAdd").into_iter().flatten() {
        if let Some(tid) = crate::subsonic::parse_track_id(raw) {
            let pos = next_position(&st.pool, pid).await;
            let _ = sqlx::query(
                "INSERT INTO playlist_items(playlist_id, track_id, position) VALUES(?, ?, ?)",
            )
            .bind(pid)
            .bind(tid)
            .bind(pos)
            .execute(&st.pool)
            .await;
        }
    }

    touch(&st.pool, pid).await;
    ok(json!({})).into_response()
}

pub async fn delete_playlist(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(id) = q.get("id").and_then(|s| parse_pl_id(s)) else {
        return error(10, "missing or invalid id");
    };
    let _ = sqlx::query("DELETE FROM playlist_items WHERE playlist_id = ?")
        .bind(id)
        .execute(&st.pool)
        .await;
    let _ = sqlx::query("DELETE FROM playlists WHERE id = ?")
        .bind(id)
        .execute(&st.pool)
        .await;
    ok(json!({})).into_response()
}
