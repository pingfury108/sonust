use std::collections::HashMap;

use axum::{
    extract::{FromRequestParts, Query},
    http::request::Parts,
    response::{IntoResponse, Response},
};
use md5::{Digest as Md5Digest, Md5};
use sha2::Sha256;
use sqlx::SqlitePool;
use tracing::info;

use crate::config::Config;
use crate::subsonic::response::error;

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

fn md5_hex(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

/// 解析 apiKey：优先 CLI/env，其次数据库中已持久化的哈希，都没有则生成并打印一次。
pub async fn resolve_api_key(pool: &SqlitePool, cfg: &Config) -> anyhow::Result<String> {
    if let Some(key) = &cfg.api_key {
        return Ok(sha256_hex(key));
    }
    if let Some(hash) = crate::db::get_setting(pool, "api_key_hash").await? {
        return Ok(hash);
    }
    let mut bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    let key = hex::encode(bytes);
    info!("generated apiKey (shown once): {key}");
    let hash = sha256_hex(&key);
    crate::db::set_setting(pool, "api_key_hash", &hash).await?;
    Ok(hash)
}

/// 校验 Subsonic 认证参数：apiKey > t/s > p（默认拒绝）。
pub fn verify(
    params: &HashMap<String, String>,
    cfg: &Config,
    api_key_hash: &str,
) -> Result<(), Response> {
    if let Some(u) = params.get("u") {
        if u != &cfg.user {
            return Err(error(40, "wrong username or password"));
        }
    }

    if let Some(k) = params.get("apiKey") {
        return if sha256_hex(k) == api_key_hash {
            Ok(())
        } else {
            Err(error(40, "wrong username or password"))
        };
    }

    if let (Some(t), Some(s)) = (params.get("t"), params.get("s")) {
        let pass = cfg
            .password
            .as_ref()
            .ok_or_else(|| error(41, "token auth requires a configured password"))?;
        return if md5_hex(&format!("{pass}{s}")).eq_ignore_ascii_case(t) {
            Ok(())
        } else {
            Err(error(40, "wrong username or password"))
        };
    }

    if let Some(p) = params.get("p") {
        if !cfg.allow_plaintext_auth {
            return Err(error(
                41,
                "plaintext password auth is disabled; use apiKey or token auth",
            ));
        }
        let plain = match p.strip_prefix("enc:") {
            Some(hexs) => hex::decode(hexs)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default(),
            None => p.clone(),
        };
        return if cfg.password.as_deref() == Some(plain.as_str()) {
            Ok(())
        } else {
            Err(error(40, "wrong username or password"))
        };
    }

    Err(error(10, "required authentication parameter missing"))
}

/// Axum extractor：所有 /rest/* 处理器的第一道防线。
pub struct SubsonicAuth;

impl FromRequestParts<crate::AppState> for SubsonicAuth {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        let Query(params): Query<HashMap<String, String>> =
            Query::from_request_parts(parts, state)
                .await
                .map_err(|_| error(10, "invalid query parameters").into_response())?;
        verify(&params, &state.cfg, &state.api_key_hash)?;
        Ok(SubsonicAuth)
    }
}
