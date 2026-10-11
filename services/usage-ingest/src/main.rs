use luxforge_usage_ingest::config::Config;
use luxforge_usage_ingest::limits::SystemClock;
use luxforge_usage_ingest::server::Server;
use std::process::ExitCode;
use std::sync::Arc;

#[tokio::main]
async fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("usage-ingest: {message}");
            return ExitCode::from(2);
        }
    };
    let sink = Arc::new(|line: &str| eprintln!("{line}"));
    let server = match Server::start(config, Arc::new(SystemClock::new()), sink).await {
        Ok(server) => server,
        Err(message) => {
            eprintln!("usage-ingest: {message}");
            return ExitCode::from(1);
        }
    };
    let _ = tokio::signal::ctrl_c().await;
    server.diagnostics().emit();
    server.shutdown().await;
    ExitCode::SUCCESS
}
