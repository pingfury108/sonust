pub mod auth;
pub mod cli;
pub mod config;
pub mod cover;
pub mod db;
pub mod dupes;
pub mod lyrics;
pub mod mirror;
pub mod scanner;
pub mod subsonic;

use std::sync::Arc;

use anyhow::Result;
use sqlx::SqlitePool;
use tracing::info;

use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub cfg: Arc<Config>,
    pub api_key_hash: String,
}

pub async fn run(cfg: Config) -> Result<()> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    std::fs::create_dir_all(cfg.data_dir.join("covers"))?;

    let pool = db::init(&cfg.data_dir).await?;
    let api_key_hash = auth::resolve_api_key(&pool, &cfg).await?;

    if cfg.password.is_none() {
        info!("no password configured: token (t/s) auth disabled, apiKey only");
    }

    let state = AppState {
        pool: pool.clone(),
        cfg: Arc::new(cfg.clone()),
        api_key_hash,
    };

    // 后台增量扫描，扫完自动补镜像
    let scan_pool = pool.clone();
    let scan_cfg = state.cfg.clone();
    tokio::spawn(async move {
        match scanner::scan_all(&scan_pool, &scan_cfg.music_dirs.clone()).await {
            Ok(stats) => info!(?stats, "scan finished"),
            Err(e) => tracing::error!(error = %e, "scan failed"),
        }
        if scan_cfg.mirror_enabled {
            match mirror::sync(&scan_pool, scan_cfg.as_ref(), false, false).await {
                Ok(m) => info!(covered = m.covered, transcoded = m.transcoded, retranscoded = m.retranscoded, removed = m.removed, failed = m.failed, "mirror sync finished"),
                Err(e) => tracing::error!(error = %e, "mirror sync failed"),
            }
        }
    });

    // 文件变化实时监听
    let watch_pool = pool.clone();
    let watch_cfg = state.cfg.clone();
    tokio::spawn(async move {
        if let Err(e) = scanner::watch(watch_pool, watch_cfg).await {
            tracing::error!(error = %e, "watcher failed");
        }
    });

    let app = subsonic::router(state);
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(%addr, "sonust listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutting down");
}
