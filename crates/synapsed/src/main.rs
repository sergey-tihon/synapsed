use clap::Parser;
use std::{fmt, io, path::PathBuf};
use synapsed::{
    config::{Config, PrivateState},
    inventory::Sources,
    logs::LogRing,
    server::{Application, RunningServer},
};
use synapsed_api::{ApiError, LogLevel};
use synapsed_core::StateStore;
use synapsed_ddc::LazyDdc;

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
        .unwrap_or_else(|_| fail_startup(&logs, StartupError::Logging));
    // Configuration and private file I/O happen before the async executor starts.
    // Keep this guard outside run so an incomplete shutdown cannot release the
    // daemon lock before fail terminates the process and its detached native work.
    let config =
        Config::load(arguments.config.as_deref()).unwrap_or_else(|error| fail(&logs, error));
    let private = PrivateState::acquire(&config.state_dir, config.name.as_deref())
        .unwrap_or_else(|error| fail(&logs, error));
    let store = StateStore::new(private.node.clone()).unwrap_or_else(|error| fail(&logs, error));
    let (host, ddc) =
        synapsed::start_plugins(store.clone(), &config).unwrap_or_else(|error| fail(&logs, error));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|_| fail_startup(&logs, StartupError::Runtime));
    if let Err(error) = runtime.block_on(run(config, &logs, store, host, ddc)) {
        fail_startup(&logs, error);
    }
}

enum StartupError {
    Logging,
    Runtime,
    Signal(&'static str, io::ErrorKind),
    Bind(io::ErrorKind),
    Daemon(ApiError),
}
impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Logging => formatter.write_str("Cannot install daemon logging."),
            Self::Runtime => formatter.write_str("Cannot start the daemon executor."),
            Self::Signal(name, kind) => write!(
                formatter,
                "Cannot install {name} signal handling ({kind:?})."
            ),
            Self::Bind(kind) => write!(
                formatter,
                "Cannot bind the configured loopback address ({kind:?})."
            ),
            Self::Daemon(error) => formatter.write_str(&error.message),
        }
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

fn fail_startup(logs: &LogRing, error: StartupError) -> ! {
    logs.record(
        LogLevel::Error,
        "synapsed",
        "Daemon failed to start or finish cleanly.",
    );
    eprintln!("synapsed: {error}");
    std::process::exit(1);
}
async fn run(
    config: Config,
    logs: &LogRing,
    store: StateStore,
    host: synapsed_plugins::PluginHost,
    ddc: LazyDdc,
) -> Result<(), StartupError> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| StartupError::Signal("termination", error.kind()))?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|error| StartupError::Signal("interrupt", error.kind()))?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|error| StartupError::Bind(error.kind()))?;
    let inventory = Sources::new(store.clone(), ddc, host.bindings(), host.catalog());
    let application = Application::new(store, host, logs.clone(), inventory);
    let mut server = RunningServer::start(listener, application, config.allowed_origins)
        .map_err(StartupError::Daemon)?;
    let result = server
        .run_until(async {
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
            }
        })
        .await;
    let shutdown = server.shutdown().await;
    result.and(shutdown).map_err(StartupError::Daemon)
}
