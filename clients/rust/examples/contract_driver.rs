//! Adapter used by the shared contract suite, not an installed command.
use flare_client::*;
use serde_json::{Value, json};
use std::io::Read;

async fn execute(client: &ApiClient, op: &str, args: Value) -> Result<Value, Error> {
    let id = || args["id"].as_str().unwrap_or_default().to_owned();
    let encode = |value| serde_json::to_value(value).unwrap();
    Ok(match op {
        "open_issue" => {
            serde_json::to_value(client.open(serde_json::from_value(args).unwrap()).await?).unwrap()
        }
        "close_issue" => serde_json::to_value(client.close(id()).await?).unwrap(),
        "get_issue" => serde_json::to_value(client.get(id()).await?).unwrap(),
        "list_issues" => serde_json::to_value(
            client
                .list(
                    args.get("status")
                        .map(|v| serde_json::from_value(v.clone()).unwrap()),
                    args["limit"].as_u64().unwrap_or(100) as u32,
                    args["offset"].as_u64().unwrap_or(0) as u32,
                )
                .await?,
        )
        .unwrap(),
        "alert" => {
            let mut args = args;
            let key = args
                .as_object_mut()
                .unwrap()
                .remove("idempotency_key")
                .map(|v| v.as_str().unwrap().to_owned());
            serde_json::to_value(
                client
                    .alert(serde_json::from_value(args).unwrap(), key)
                    .await?,
            )
            .unwrap()
        }
        "get_delivery" => {
            serde_json::to_value(client.delivery(args["id"].as_i64().unwrap()).await?).unwrap()
        }
        "register_heartbeat" => serde_json::to_value(
            client
                .register_heartbeat(serde_json::from_value(args).unwrap())
                .await?,
        )
        .unwrap(),
        "check_in" => serde_json::to_value(client.check_in(id()).await?).unwrap(),
        "list_heartbeats" => serde_json::to_value(client.heartbeats().await?).unwrap(),
        "delete_heartbeat" => {
            client.delete_heartbeat(id()).await?;
            Value::Null
        }
        "health" => serde_json::to_value(client.health().await?).unwrap(),
        "readiness" => serde_json::to_value(client.readiness().await?).unwrap(),
        "metrics" => encode(client.metrics().await?),
        _ => panic!("Unknown contract operation"),
    })
}

#[tokio::main]
async fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    let request: Value = serde_json::from_str(&input).unwrap();
    let client = ApiClient::with_timeout(
        std::env::var("FLARE_BASE_URL").unwrap(),
        std::env::var("FLARE_API_TOKEN").unwrap(),
        std::time::Duration::from_secs_f64(
            std::env::var("FLARE_TIMEOUT")
                .unwrap_or("15".into())
                .parse()
                .unwrap(),
        ),
    );
    let result = match client {
        Ok(client) => {
            execute(
                &client,
                request["op"].as_str().unwrap(),
                request["args"].clone(),
            )
            .await
        }
        Err(error) => Err(error),
    };
    let output = match result {
        Ok(value) => json!({"value":value}),
        Err(error) => match error {
            Error::Http { status } => json!({"error":"http","status":status}),
            Error::Validation(_) => json!({"error":"validation"}),
            Error::Transport => json!({"error":"transport"}),
            Error::Decode => json!({"error":"decode"}),
        },
    };
    println!("{output}");
}
