use axum::response::{Json, Response};
use serde_json::{json, Value};

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

/// 声明支持的 OpenSubsonic 扩展（当前无扩展，仅表明兼容身份）。
pub async fn get_extensions(_auth: SubsonicAuth) -> Json<Value> {
    ok(json!({
        "openSubsonicExtensions": {
            "openSubsonicExtension": []
        }
    }))
}

pub async fn not_found() -> Response {
    crate::subsonic::response::error(0, "unknown endpoint")
}
