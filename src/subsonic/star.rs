use std::collections::HashMap;

use axum::{
    extract::{RawQuery, State},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use sqlx::Row;

use crate::auth::SubsonicAuth;
use crate::subsonic::browsing::{album_json, song_json, ALBUM_EXTRA_SQL, SONG_SQL};
use crate::subsonic::response::{error, ok};
use crate::subsonic::{artist_id, parse_album_id, parse_artist_id, parse_track_id};
use crate::AppState;

/// 手动解析重复参数（id=1&id=2）。
pub(crate) fn parse_multi(raw: &str) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (k, v) in form_urlencoded::parse(raw.as_bytes()) {
        map.entry(k.into_owned()).or_default().push(v.into_owned());
    }
    map
}

/// star/unstar 的参数可重复出现（id=1&id=2），所以手工解析。
async fn set_star(
    st: &AppState,
    q: &HashMap<String, Vec<String>>,
    starred: bool,
) -> Response {
    let kinds = [
        ("id", "track", parse_track_id as fn(&str) -> Option<i64>),
        ("albumId", "album", parse_album_id),
        ("artistId", "artist", parse_artist_id),
    ];
    for (param, item_type, parse) in kinds {
        for raw in q.get(param).into_iter().flatten() {
            let Some(id) = parse(raw) else {
                return error(10, &format!("invalid {param}: {raw}"));
            };
            if let Err(e) = crate::db::set_starred(&st.pool, item_type, id, starred).await {
                return error(0, &e.to_string());
            }
        }
    }
    ok(json!({})).into_response()
}

pub async fn star(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    RawQuery(raw): RawQuery,
) -> Response {
    set_star(&st, &parse_multi(raw.as_deref().unwrap_or("")), true).await
}

pub async fn unstar(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    RawQuery(raw): RawQuery,
) -> Response {
    set_star(&st, &parse_multi(raw.as_deref().unwrap_or("")), false).await
}

/// getStarred 与 getStarred2 共用，仅响应键名不同。
pub async fn get_starred(st: AppState, key: &str) -> Response {
    let artists = sqlx::query(
        "SELECT ar.id, ar.name, s.created,
                (SELECT COUNT(*) FROM albums al WHERE al.artist_id = ar.id) AS album_count
         FROM starred s JOIN artists ar ON ar.id = s.item_id
         WHERE s.item_type = 'artist' ORDER BY s.created DESC",
    )
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    let albums = sqlx::query(&format!(
        "SELECT al.*, ar.name AS artist_name, s.created AS starred,
                (SELECT COUNT(*) FROM tracks t WHERE t.album_id = al.id) AS song_count,
                (SELECT COALESCE(SUM(t.duration), 0.0) FROM tracks t WHERE t.album_id = al.id) AS duration,
                {ALBUM_EXTRA_SQL}
         FROM starred s
         JOIN albums al ON al.id = s.item_id
         LEFT JOIN artists ar ON ar.id = al.artist_id
         WHERE s.item_type = 'album' ORDER BY s.created DESC"
    ))
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    let songs = sqlx::query(&format!(
        "{SONG_SQL} JOIN starred s ON s.item_type = 'track' AND s.item_id = t.id
         WHERE s.created IS NOT NULL ORDER BY s.created DESC"
    ))
    .fetch_all(&st.pool)
    .await
    .unwrap_or_default();

    ok(json!({
        key: {
            "artist": artists.iter().map(|r| json!({
                "id": artist_id(r.get::<i64, _>("id")),
                "name": r.get::<String, _>("name"),
                "albumCount": r.get::<i64, _>("album_count"),
                "starred": r.get::<String, _>("created"),
            })).collect::<Vec<_>>(),
            "album": albums.iter().map(|r| album_json(r, r.get("song_count"), r.get::<f64, _>("duration") as i64)).collect::<Vec<Value>>(),
            "song": songs.iter().map(song_json).collect::<Vec<Value>>(),
        }
    }))
    .into_response()
}

pub async fn get_starred2_handler(State(st): State<AppState>, _auth: SubsonicAuth) -> Response {
    get_starred(st, "starred2").await
}

pub async fn get_starred_handler(State(st): State<AppState>, _auth: SubsonicAuth) -> Response {
    get_starred(st, "starred").await
}
