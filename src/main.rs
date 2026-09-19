use clap::Parser;
use zoder::config::Config;

#[derive(Parser, Debug)]
#[command(name = "zoder", about = "Terminal coding assistant", version)]
struct Cli {
    /// Prompt to send immediately
    prompt: Vec<String>,
    /// Provider id from config.toml (`[providers.<id>]`)
    #[arg(long)]
    provider: Option<String>,
    /// Inference host, including scheme and port
    #[arg(long)]
    host: Option<String>,
    /// Model id on the active provider
    #[arg(long)]
    model: Option<String>,
    /// Resume the most recent session for this directory
    #[arg(short = 'c', long)]
    continue_last: bool,
}

#[tokio::main]
async fn main() {
    if let Err(e) = start().await {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}

async fn start() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut cfg = Config::load().unwrap_or_default();
    if let Some(id) = cli.provider {
        if !cfg.select_provider(&id) {
            anyhow::bail!("unknown provider `{id}` (see ~/.zoder/config.toml)");
        }
    }
    if let Some(host) = cli.host {
        cfg.set_host(zoder::config::normalize_host(&host));
    }
    if let Some(model) = cli.model {
        cfg.set_model(model);
    }
    let prompt = cli.prompt.join(" ");
    if !prompt.is_empty() {
        std::env::set_var("ZODER_BOOT_PROMPT", &prompt);
    }
    if cli.continue_last {
        std::env::set_var("ZODER_CONTINUE", "1");
    }
    zoder::run(cfg).await
}
