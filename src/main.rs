#[tokio::main]
async fn main() -> anyhow::Result<()> {
    sonust::cli::main().await
}
