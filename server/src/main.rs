//! parakeet-server-rs — a universal local transcription server.
//!
//! The C++/Vulkan engine (`parakeet-server`) keeps the model warm; this process owns the
//! HTTP API, the single-GPU queue, the correction rules and the transcript cache.

mod api;
mod config;
mod corrections;
mod engine;
mod formats;
mod queue;
mod store;

use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    config::init_logging();

    let mut config_path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => config_path = args.next().map(PathBuf::from),
            "--version" | "-V" => {
                println!("parakeet-server-rs {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other} (try --help)"),
        }
    }

    let cfg = Arc::new(config::Config::load(config_path.as_deref())?);
    tracing::info!(bind = %cfg.bind, model = %cfg.model_id, "starting parakeet-server-rs");

    let store = Arc::new(store::Store::open(&cfg.database)?);
    match store.purge(cfg.retention_days) {
        Ok(n) if n > 0 => tracing::info!(removed = n, "purged old jobs"),
        _ => {}
    }

    let engine = engine::Engine::new(cfg.engine.clone());
    tokio::spawn(engine.clone().supervise());
    if let Err(err) = engine.ensure().await {
        // Not fatal: requests retry the start, and the supervisor keeps trying.
        tracing::warn!(%err, "engine is not ready yet");
    }

    let queue = queue::Queue::start(cfg.clone(), store.clone(), engine.clone());
    let state = api::AppState { cfg: cfg.clone(), store, engine, queue };

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!("listening on http://{}", cfg.bind);
    axum::serve(listener, api::router(state)).await?;
    Ok(())
}

fn print_help() {
    println!(
        "parakeet-server-rs {}\n\
         \n\
         Usage: parakeet-server-rs [--config <path>]\n\
         \n\
         Options:\n\
           -c, --config <path>  JSON configuration (default {})\n\
           -V, --version        Print the version\n\
           -h, --help           Print this help\n\
         \n\
         Environment:\n\
           PARAKEET_BIND        Overrides the listen address\n\
           PARAKEET_MODEL_ID     Overrides the model name reported by /v1/models\n\
           PARAKEET_LOG          Log filter (default \"info\")\n",
        env!("CARGO_PKG_VERSION"),
        config::DEFAULT_CONFIG_PATH
    );
}
