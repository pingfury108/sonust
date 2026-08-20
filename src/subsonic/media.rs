use std::collections::HashMap;

use axum::{
    extract::{Query, Request, State},
    response::Response,
};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use axum::response::IntoResponse;

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

pub async fn get_cover_art(
    State(st): State<AppState>,
    _auth: SubsonicAuth,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
) -> Response {
    let Some(id) = q.get("id") else {
        return error(10, "missing id");
    };
    let size: Option<u32> = q.get("size").and_then(|s| s.parse().ok());
    match crate::cover::cover_file(&st.pool, &st.cfg.data_dir, &st.cfg.music_dirs, id, size).await {
        Ok(Some(path)) => serve_file(req, path).await,
        Ok(None) => error(70, "cover not found"),
        Err(e) => error(0, &e.to_string()),
    }
}
