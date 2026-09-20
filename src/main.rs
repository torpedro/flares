use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use flares::{
    api,
    client::ApiClient,
    config::{ClientConfig, ServerConfig},
    delivery::DeliveryService,
    models::*,
    store::Store,
};

#[derive(Parser)]
#[command(version, about = "Track issues and send notifications")]
struct Cli {
    /// YAML file; otherwise search $XDG_CONFIG_HOME/flares (or ~/.config/flares), then /etc/flares.
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
    /// Validate configuration or display resolved settings with secrets redacted.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Run the HTTP service.
    Serve,
    /// Send a one-shot alert.
    Alert {
        #[arg(long)]
        title: String,
        #[arg(long)]
        message: String,
        #[arg(long, value_enum, default_value_t = Severity::Warning)]
        severity: Severity,
        #[arg(long)]
        idempotency_key: Option<String>,
        #[arg(long)]
        group_key: Option<String>,
    },
    /// Inspect a durable delivery and its destination outcomes.
    Delivery { id: i64 },
    /// Manage expected check-ins from jobs and services.
    Heartbeat {
        #[command(subcommand)]
        command: HeartbeatCommand,
    },
    /// Open or reopen an issue. An already-open issue is unchanged.
    Open {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        message: Option<String>,
        #[arg(long, value_enum, default_value_t = Severity::Warning)]
        severity: Severity,
        #[arg(long)]
        remind_every_seconds: Option<u64>,
        #[arg(long)]
        notify_on_resolution: bool,
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

#[derive(Subcommand)]
enum ConfigCommand {
    /// Validate configuration without opening the database or contacting providers.
    Check {
        #[arg(long)]
        client: bool,
    },
    /// Display effective configuration with all secrets redacted.
    Show {
        #[arg(long)]
        client: bool,
    },
}

#[derive(Subcommand)]
enum HeartbeatCommand {
    /// Create or replace a monitor; starts its deadline now.
    Add {
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        interval_seconds: u64,
        #[arg(long, default_value_t = 0)]
        grace_seconds: u64,
        #[arg(long, value_enum, default_value_t=Severity::Warning)]
        severity: Severity,
        #[arg(long)]
        notify_on_recovery: bool,
    },
    /// Record a successful check-in.
    Beat {
        id: String,
    },
    List,
    Remove {
        id: String,
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
    let path = match cli.config {
        Some(path) => path,
        None => flares::config::default_path(
            if matches!(
                cli.command,
                Command::Serve
                    | Command::Config {
                        command: ConfigCommand::Check { client: false }
                            | ConfigCommand::Show { client: false }
                    }
            ) {
                "server.yaml"
            } else {
                "client.yaml"
            },
        )?,
    };
    if let Command::Config { command } = &cli.command {
        let client = matches!(
            command,
            ConfigCommand::Check { client: true } | ConfigCommand::Show { client: true }
        );
        let value = if client {
            ClientConfig::load(&path)?.effective()
        } else {
            ServerConfig::load(&path)?.effective()
        };
        match command {
            ConfigCommand::Check { .. } => {
                if cli.json {
                    println!("{}", serde_json::json!({"valid": true}));
                } else {
                    println!("Configuration is valid");
                }
            }
            ConfigCommand::Show { .. } => {
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&value)?);
                } else {
                    print!("{}", serde_yaml_ng::to_string(&value)?);
                }
            }
        }
        return Ok(0);
    }
    if matches!(cli.command, Command::Serve) {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_max_level(tracing::Level::INFO)
            .init();
        let config = ServerConfig::load(&path)?;
        let store = Store::open_file(&config.database)?;
        let delivery = DeliveryService::configured(store, &config)?;
        let app = api::router_with_delivery(delivery.clone(), config.api_token);
        let listener = tokio::net::TcpListener::bind((config.host, config.port))
            .await
            .map_err(|_| anyhow::anyhow!("Cannot bind HTTP listener; check host and port"))?;
        tracing::info!(address = %listener.local_addr()?, "Flares listening");
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let worker = tokio::spawn(delivery.run(receiver));
        let signal = stop.clone();
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown().await;
                let _ = signal.send(true);
            })
            .await;
        let _ = stop.send(true);
        worker.await?;
        result?;
        return Ok(0);
    }
    let config = ClientConfig::load(&path)?;
    let client = ApiClient::with_timeout(
        config.base_url,
        config.api_token.expose(),
        std::time::Duration::from_secs_f64(config.timeout),
    )?;
    match cli.command {
        Command::Alert {
            title,
            message,
            severity,
            idempotency_key,
            group_key,
        } => {
            let result = client
                .alert(
                    Alert {
                        title,
                        message,
                        severity,
                        group_key,
                    },
                    idempotency_key,
                )
                .await?;
            if cli.json {
                println!("{}", serde_json::to_string(&result)?);
            } else {
                println!(
                    "Delivery {}; notification {}",
                    result.delivery_id,
                    result.notification.status.as_str()
                );
                if let Some(error) = &result.notification.error {
                    eprintln!("{error}");
                }
            }
            Ok(
                if result.notification.status == NotificationStatus::Failed {
                    2
                } else {
                    0
                },
            )
        }
        Command::Delivery { id } => {
            let result = client.delivery(id).await?;
            println!(
                "{}",
                if cli.json {
                    serde_json::to_string(&result)?
                } else {
                    serde_json::to_string_pretty(&result)?
                }
            );
            Ok(0)
        }
        Command::Heartbeat { command } => {
            let result = match command {
                HeartbeatCommand::Add {
                    id,
                    title,
                    interval_seconds,
                    grace_seconds,
                    severity,
                    notify_on_recovery,
                } => serde_json::to_value(
                    client
                        .register_heartbeat(HeartbeatInput {
                            id,
                            title,
                            interval_seconds,
                            grace_seconds,
                            severity,
                            notify_on_recovery,
                        })
                        .await?,
                )?,
                HeartbeatCommand::Beat { id } => serde_json::to_value(client.check_in(id).await?)?,
                HeartbeatCommand::List => serde_json::to_value(client.heartbeats().await?)?,
                HeartbeatCommand::Remove { id } => {
                    client.delete_heartbeat(id).await?;
                    serde_json::json!({"deleted":true})
                }
            };
            println!(
                "{}",
                if cli.json {
                    serde_json::to_string(&result)?
                } else {
                    serde_json::to_string_pretty(&result)?
                }
            );
            Ok(0)
        }
        Command::Open {
            id,
            title,
            message,
            severity,
            remind_every_seconds,
            notify_on_resolution,
        } => print_mutation(
            client
                .open(OpenIssue {
                    id,
                    title,
                    message,
                    severity,
                    remind_every_seconds,
                    notify_on_resolution,
                })
                .await?,
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
        Command::Serve | Command::Config { .. } => unreachable!(),
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
