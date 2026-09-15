use std::{path::PathBuf, process::ExitCode, sync::Arc};

use clap::{Parser, Subcommand};
use flare::{
    api,
    client::ApiClient,
    config::{ClientConfig, ServerConfig},
    models::*,
    notifications::{Notifier, Pushover},
    store::Store,
};

#[derive(Parser)]
#[command(version, about = "Track issues and send Pushover notifications")]
struct Cli {
    /// YAML configuration (defaults to server.yaml for serve, client.yaml otherwise).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP service.
    Serve,
    /// Open or reopen an issue. An already-open issue is unchanged.
    Open {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        message: Option<String>,
    },
    /// Close an existing issue.
    Close { id: String },
    /// Inspect an issue.
    Get { id: String },
    /// List issues, newest updated first.
    List {
        #[arg(long, value_enum)]
        status: Option<IssueStatus>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u32,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() { 1 } else { 0 };
            let _ = error.print();
            return ExitCode::from(code);
        }
    };
    let json = cli.json;
    match run(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if json {
                println!("{}", serde_json::json!({"error": error.to_string()}));
            } else {
                eprintln!("Error: {error}");
            }
            ExitCode::from(1)
        }
    }
}

fn print_issue(issue: &Issue) {
    // Debug-quote user text so IDs/titles containing newlines or escape codes stay on one line.
    println!(
        "{:?}\t{}\t{:?}\topenings={}\tnotification={}",
        issue.id,
        issue.status.as_str(),
        issue.title,
        issue.opening_count,
        issue.notification.status.as_str()
    );
}

fn print_mutation(result: MutationResult, json: bool) -> anyhow::Result<u8> {
    if json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        print_issue(&result.issue);
        println!(
            "{}; notification {}",
            if result.changed {
                "Changed"
            } else {
                "Unchanged"
            },
            result.notification.status.as_str()
        );
    }
    if result.notification.status == NotificationStatus::Failed {
        if !json {
            eprintln!(
                "Issue saved, but notification failed: {}",
                result
                    .notification
                    .error
                    .as_deref()
                    .unwrap_or("unknown error")
            );
        }
        Ok(2)
    } else {
        Ok(0)
    }
}

async fn run(cli: Cli) -> anyhow::Result<u8> {
    let path = cli.config.unwrap_or_else(|| {
        if matches!(cli.command, Command::Serve) {
            "server.yaml".into()
        } else {
            "client.yaml".into()
        }
    });
    if matches!(cli.command, Command::Serve) {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_max_level(tracing::Level::INFO)
            .init();
        let config = ServerConfig::load(&path)?;
        let store = Store::open_file(&config.database)?;
        let notifier = config
            .pushover
            .map(Pushover::new)
            .transpose()?
            .map(|channel| Arc::new(channel) as Arc<dyn Notifier>);
        let app = api::router(store, notifier, config.api_token);
        let listener = tokio::net::TcpListener::bind((config.host, config.port))
            .await
            .map_err(|_| anyhow::anyhow!("Cannot bind HTTP listener; check host and port"))?;
        tracing::info!(address = %listener.local_addr()?, "Flare listening");
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown())
            .await?;
        return Ok(0);
    }
    let client = ApiClient::new(ClientConfig::load(&path)?)?;
    match cli.command {
        Command::Open { id, title, message } => print_mutation(
            client.open(OpenIssue { id, title, message }).await?,
            cli.json,
        ),
        Command::Close { id } => print_mutation(client.close(id).await?, cli.json),
        Command::Get { id } => {
            let issue = client.get(id).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&issue)?);
            } else {
                print_issue(&issue);
                println!(
                    "Message: {:?}\nCreated: {}\nUpdated: {}\nOpened: {}",
                    issue.message, issue.created_at, issue.updated_at, issue.opened_at
                );
                if let Some(closed) = issue.closed_at {
                    println!("Last closed: {closed}");
                }
                if let Some(error) = issue.notification.error {
                    println!("Notification error: {error:?}");
                }
            }
            Ok(0)
        }
        Command::List {
            status,
            limit,
            offset,
        } => {
            let list = client.list(status, limit, offset).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&list)?);
            } else {
                for issue in &list.items {
                    print_issue(issue);
                }
                println!(
                    "{} of {} issues (offset {})",
                    list.items.len(),
                    list.total,
                    list.offset
                );
            }
            Ok(0)
        }
        Command::Serve => unreachable!(),
    }
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
