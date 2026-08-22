use axum::{
    extract::State,
    response::{IntoResponse, Json, Response},
};
use serde_json::{json, Value};
use sqlx::Row;
use tracing::info;

use crate::auth::SubsonicAuth;
use crate::subsonic::response::ok;

pub async fn ping(_auth: SubsonicAuth) -> Json<Value> {
    ok(json!({}))
}

pub async fn get_license(_auth: SubsonicAuth) -> Json<Value> {
    ok(json!({
        "license": {
            "valid": true,
            "email": "user@sonust.local",
            "licenseExpires": "2099-12-31T23:59:59Z"
        }
    }))
}

/// 声明支持的 OpenSubsonic 扩展。
pub async fn get_extensions(_auth: SubsonicAuth) -> Json<Value> {
    ok(json!({
        "openSubsonicExtensions": {
            "openSubsonicExtension": [
                { "name": "songLyrics", "versions": [1] }
            ]
        }
    }))
}

/// 手动触发服务端重扫（Tempus 的"扫描曲库"按钮调用）。
pub async fn start_scan(State(st): State<crate::AppState>, _auth: SubsonicAuth) -> Response {
    let pool = st.pool.clone();
    let dirs = st.cfg.music_dirs.clone();
    tokio::spawn(async move {
        match crate::scanner::scan_all(&pool, &dirs).await {
            Ok(s) => info!(?s, "manual scan done"),
            Err(e) => tracing::warn!(error = %e, "manual scan failed"),
        }
    });
    ok(json!({
        "scanStatus": {"scanning": true, "count": 0, "folderCount": 0, "lastScan": ""}
    }))
    .into_response()
}

/// 扫描状态查询。
pub async fn get_scan_status(State(st): State<crate::AppState>, _auth: SubsonicAuth) -> Response {
    let count: i64 = sqlx::query("SELECT COUNT(*) FROM tracks")
        .fetch_one(&st.pool)
        .await
        .map(|r| r.get(0))
        .unwrap_or(0);
    let last_scan: Option<i64> = sqlx::query("SELECT MAX(mtime) FROM tracks")
        .fetch_one(&st.pool)
        .await
        .map(|r| r.get::<Option<i64>, _>(0))
        .unwrap_or(None);
    ok(json!({
        "scanStatus": {
            "scanning": false,
            "count": count,
            "folderCount": st.cfg.music_dirs.len(),
            "lastScan": last_scan
        }
    }))
    .into_response()
}

pub async fn not_found() -> Response {
    crate::subsonic::response::error(0, "unknown endpoint")
}
