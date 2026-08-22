use std::collections::HashMap;

use axum::{
    extract::{Query, Request, State},
    response::Response,
};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use axum::response::IntoResponse;
use sqlx::Row;

use crate::auth::SubsonicAuth;
use crate::subsonic::response::error;
use crate::AppState;

/// 输出原始文件流；ServeFile 自动处理 Range / 206 Partial Content。
async fn serve_file(req: Request, path: std::path::PathBuf) -> Response {
    match ServeFile::new(&path).oneshot(req).await {
        Ok(res) => res.into_response(),
        Err(e) => error(0, &format!("failed to serve {}: {e}", path.display())),
    }
}

pub async fn stream(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
) -> Response {
    let Some(id) = q.get("id") else {
        return error(10, "missing id");
    };
    match crate::cover::track_path_for(&st.pool, &st.cfg.music_dirs, id).await {
        Ok(Some(path)) => serve_file(req, path).await,
        Ok(None) => error(70, "media not found"),
        Err(e) => error(0, &e.to_string()),
    }
}

/// getCoverArt：ar-/music/dir- id 归一化到实体下第一首有封面的曲目，
/// 使歌手封面、音乐库根封面也能正常返回，避免 Tempus 等客户端无封面。
async fn resolve_cover_id(st: &AppState, id: &str) -> Option<String> {
    if crate::subsonic::parse_track_id(id).is_some()
        || crate::subsonic::parse_album_id(id).is_some()
    {
        return Some(id.to_string());
    }

    // 歌手：取该歌手下任一有封面的曲目
    if let Some(aid) = crate::subsonic::parse_artist_id(id) {
        let row = sqlx::query(
            "SELECT t.id FROM tracks t WHERE t.artist_id = ? AND t.has_cover = 1 ORDER BY t.id LIMIT 1",
        )
        .bind(aid)
        .fetch_optional(&st.pool)
        .await
        .ok()
        .flatten();
        return row.map(|r| crate::subsonic::track_id(r.get::<i64, _>("id")));
    }

    // 音乐库根（Tempus 请求 id=music）与目录 id：取该目录下任一有封面的曲目
    let (folder, prefix) = if id == "music" || id == "0" {
        (0i64, String::new())
    } else if let Some((f, p)) = crate::subsonic::directory::parse_dir_id_pub(id) {
        (f, p)
    } else {
        return None;
    };

    let row = if prefix.is_empty() {
        sqlx::query("SELECT t.id FROM tracks t WHERE t.folder = ? AND t.has_cover = 1 ORDER BY t.id LIMIT 1")
            .bind(folder)
            .fetch_optional(&st.pool)
            .await
            .ok()
            .flatten()
    } else {
        let pattern = format!("{}/%", crate::subsonic::directory::like_escape_pub(&prefix));
        sqlx::query(
            "SELECT t.id FROM tracks t WHERE t.folder = ? AND t.path LIKE ? ESCAPE '\\' AND t.has_cover = 1 ORDER BY t.id LIMIT 1",
        )
        .bind(folder)
        .bind(pattern)
        .fetch_optional(&st.pool)
        .await
        .ok()
        .flatten()
    };
    row.map(|r| crate::subsonic::track_id(r.get::<i64, _>("id")))
}

pub async fn get_cover_art(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
) -> Response {
    let Some(id) = q.get("id") else {
        return error(10, "missing id");
    };
    let Some(cover_id) = resolve_cover_id(&st, id).await else {
        return error(70, "cover not found");
    };
    let size: Option<u32> = q.get("size").and_then(|s| s.parse().ok());
    match crate::cover::cover_file(&st.pool, &st.cfg.data_dir, &st.cfg.music_dirs, &cover_id, size)
        .await
    {
        Ok(Some(path)) => serve_file(req, path).await,
        Ok(None) => error(70, "cover not found"),
        Err(e) => error(0, &e.to_string()),
    }
}
