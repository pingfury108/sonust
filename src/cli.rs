use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use crate::config::Config;

#[derive(Parser)]
#[command(name = "sonust", version, about = "Minimal OpenSubsonic music streaming server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// 音乐库根目录（可多次指定）
    #[arg(short, long, env = "SONUST_MUSIC_DIR")]
    music_dir: Vec<PathBuf>,

    /// 数据目录（数据库 + 封面缓存）
    #[arg(long, env = "SONUST_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// 监听地址
    #[arg(long, env = "SONUST_HOST", default_value = "0.0.0.0")]
    host: String,

    /// 监听端口
    #[arg(short, long, env = "SONUST_PORT", default_value = "4533")]
    port: u16,

    /// 用户名
    #[arg(short, long, env = "SONUST_USER", default_value = "admin")]
    user: String,

    /// 密码（仅驻留内存，优先用环境变量 SONUST_PASSWORD）
    #[arg(long, env = "SONUST_PASSWORD")]
    password: Option<String>,

    /// API Key（不指定则自动生成并持久化哈希）
    #[arg(long, env = "SONUST_API_KEY")]
    api_key: Option<String>,

    /// 允许 p= 明文密码认证（不安全，仅兼容古董客户端）
    #[arg(long, env = "SONUST_ALLOW_PLAINTEXT_AUTH")]
    allow_plaintext_auth: bool,

    /// 详细日志
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Command {
    /// 启动服务（默认）
    Serve,
    /// 执行一次扫描后退出
    Scan,
    /// 重复歌曲检测报告（A），可选隔离执行（C）
    Dupes {
        /// 非保留副本移动到该目录（不删除，留后悔药）
        #[arg(long)]
        quarantine: Option<std::path::PathBuf>,
    },
}

pub async fn main() -> Result<()> {
    let cli = Cli::parse();

    let level = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| format!("sonust={level},tower_http=info").into()),
        )
        .init();

    let data_dir = cli
        .data_dir
        .or_else(|| dirs::data_dir().map(|d| d.join("sonust")))
        .expect("cannot determine data directory");

    if cli.music_dir.is_empty() {
        bail!("at least one --music-dir (or SONUST_MUSIC_DIR) is required");
    }

    let cfg = Config {
        music_dirs: cli.music_dir,
        data_dir,
        host: cli.host,
        port: cli.port,
        user: cli.user,
        password: cli.password,
        api_key: cli.api_key,
        allow_plaintext_auth: cli.allow_plaintext_auth,
    };

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => crate::run(cfg).await,
        Command::Scan => {
            std::fs::create_dir_all(&cfg.data_dir)?;
            let pool = crate::db::init(&cfg.data_dir).await?;
            let stats = crate::scanner::scan_all(&pool, &cfg.music_dirs).await?;
            println!("scan finished: {stats:?}");
            Ok(())
        }
        Command::Dupes { quarantine } => {
            let pool = crate::db::init(&cfg.data_dir).await?;
            let groups = crate::dupes::find_groups(&pool, &cfg.music_dirs).await?;
            if groups.is_empty() {
                println!("未发现重复歌曲");
                return Ok(());
            }
            crate::dupes::print_report(&groups);
            if let Some(dir) = quarantine {
                let (n, bytes) = crate::dupes::quarantine(&groups, &cfg.music_dirs, &dir).await?;
                println!("已移动 {n} 个文件到 {}，释放约 {} MB", dir.display(), bytes / 1048576);
            } else {
                println!("\n报告模式未改动任何文件；加 --quarantine <目录> 执行隔离");
            }
            Ok(())
        }
    }
}
