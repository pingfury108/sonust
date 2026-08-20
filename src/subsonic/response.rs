use axum::response::{IntoResponse, Json, Response};
use serde_json::{json, Map, Value};

/// 标准 OpenSubsonic 响应外壳：业务字段与 status/version 平级。
pub fn ok(data: Value) -> Json<Value> {
    let mut payload = Map::new();
    payload.insert("status".into(), json!("ok"));
    payload.insert("version".into(), json!("1.16.1"));
    payload.insert("type".into(), json!("sonust"));
    payload.insert("serverVersion".into(), json!(env!("CARGO_PKG_VERSION")));
    payload.insert("openSubsonic".into(), json!(true));
    if let Value::Object(m) = data {
        payload.extend(m);
    }
    Json(json!({ "subsonic-response": payload }))
}

/// Subsonic 错误一律 HTTP 200 + 协议错误体。
pub fn error(code: i64, message: &str) -> Response {
    tracing::warn!(code, message, "subsonic error response");
    Json(json!({
        "subsonic-response": {
            "status": "failed",
            "version": "1.16.1",
            "type": "sonust",
            "serverVersion": env!("CARGO_PKG_VERSION"),
            "openSubsonic": true,
            "error": { "code": code, "message": message }
        }
    }))
    .into_response()
}
