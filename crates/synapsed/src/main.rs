use clap::Parser;
use std::path::PathBuf;
use synapsed::{
    config::{Config, PrivateState},
    logs::LogRing,
    server::{Application, RunningServer},
};
use synapsed_api::{ApiError, ErrorCode, LogLevel};
use synapsed_core::StateStore;

#[derive(Parser)]
#[command(
    name = "synapsed",
    version,
    about = "Local Lua hardware-plugin service"
)]
struct Arguments {
    /// Select the daemon TOML configuration (explicit missing files are errors).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}
fn main() {
    let arguments = Arguments::parse();
    let logs = LogRing::new(true);
    logs.install_tracing()
        .unwrap_or_else(|error| fail(&logs, error));
    // Configuration and private file I/O happen before the async executor starts.
    // Keep this guard outside run so an incomplete shutdown cannot release the
    // daemon lock before fail terminates the process and its detached native work.
    let config =
        Config::load(arguments.config.as_deref()).unwrap_or_else(|error| fail(&logs, error));
    let private = PrivateState::acquire(&config.state_dir, config.name.as_deref())
        .unwrap_or_else(|error| fail(&logs, error));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|_| {
            fail(
                &logs,
                ApiError::new(
                    ErrorCode::DeviceUnavailable,
                    "Cannot start the daemon executor.",
                ),
            )
        });
    if let Err(error) = runtime.block_on(run(config, &private, &logs)) {
        fail(&logs, error);
    }
}
fn fail(logs: &LogRing, error: ApiError) -> ! {
    logs.record(
        LogLevel::Error,
        "synapsed",
        "Daemon failed to start or finish cleanly.",
    );
    eprintln!("synapsed: {}", error.message);
    std::process::exit(1);
}
async fn run(config: Config, private: &PrivateState, logs: &LogRing) -> Result<(), ApiError> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidRequest,
            "Cannot install termination signal handling.",
        )
    })?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidRequest,
            "Cannot install interrupt signal handling.",
        )
    })?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidRequest,
                "Cannot bind the configured loopback address.",
            )
        })?;
    let store = StateStore::new(private.node.clone())?;
    let host = synapsed::start_plugins(store.clone(), &config)?;
    let application = Application::new(store, host, logs.clone());
    let mut server = RunningServer::start(listener, application, config.allowed_origins)?;
    let result = server
        .run_until(async {
            tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {} }
        })
        .await;
    let shutdown = server.shutdown().await;
    result.and(shutdown)
}
