use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub music_dirs: Vec<PathBuf>,
    pub data_dir: PathBuf,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// 仅驻留内存，永不落盘（t/s 验算需要明文）
    pub password: Option<String>,
    pub api_key: Option<String>,
    pub allow_plaintext_auth: bool,
}
