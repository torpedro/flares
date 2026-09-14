use std::{
    path::Path,
    process::{Command, Output},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use happer::{api, models::Notification, notifications::Notifier, store::Store};
use serde_json::{Value, json};

struct FakeNotifier;
#[async_trait]
impl Notifier for FakeNotifier {
    async fn send(&self, title: &str, _message: &str) -> Notification {
        if title == "fail" {
            Notification::failed("Provider unavailable")
        } else {
            Notification::sent()
        }
    }
}

async fn cli(directory: &Path, args: &[&str]) -> Output {
    let directory = directory.to_owned();
    let args: Vec<_> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_happer"))
            .current_dir(directory)
            .args(args)
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

fn result(output: Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn commands_exercise_real_http_and_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_file(&dir.path().join("db.sqlite3")).unwrap();
    let app = api::router(
        store,
        Some(Arc::new(FakeNotifier)),
        serde_json::from_value(json!("shared-token")).unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = format!("api_token: shared-token\nbase_url: http://{address}\ntimeout: 2\n");
    std::fs::write(dir.path().join("client.yaml"), &config).unwrap();

    let opened = result(
        cli(
            dir.path(),
            &[
                "--json",
                "open",
                "backup",
                "--title",
                "Backup failed",
                "--message",
                "Please check",
            ],
        )
        .await,
        0,
    );
    assert_eq!(opened["issue"]["message"], "Please check");
    assert_eq!(opened["notification"]["status"], "sent");
    let duplicate = result(cli(dir.path(), &["open", "backup", "--json"]).await, 0);
    assert_eq!(duplicate["changed"], false);
    assert_eq!(
        result(cli(dir.path(), &["get", "backup", "--json"]).await, 0),
        opened["issue"]
    );
    let list = result(
        cli(
            dir.path(),
            &[
                "--json", "list", "--status", "open", "--limit", "1", "--offset", "0",
            ],
        )
        .await,
        0,
    );
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], "backup");
    assert_eq!(
        result(cli(dir.path(), &["--json", "close", "backup"]).await, 0)["changed"],
        true
    );
    assert_eq!(
        result(cli(dir.path(), &["--json", "close", "backup"]).await, 0)["changed"],
        false
    );
    assert_eq!(
        result(cli(dir.path(), &["--json", "open", "backup"]).await, 0)["issue"]["opening_count"],
        2
    );

    let failed = result(
        cli(
            dir.path(),
            &[
                "--config",
                "client.yaml",
                "--json",
                "open",
                "failed",
                "--title",
                "fail",
            ],
        )
        .await,
        2,
    );
    assert_eq!(failed["issue"]["status"], "open");
    assert_eq!(failed["notification"]["status"], "failed");
    assert_eq!(
        result(cli(dir.path(), &["--json", "open", "failed"]).await, 0)["notification"]["status"],
        "not_attempted"
    );
    let human = cli(dir.path(), &["open", "another-failure", "--title", "fail"]).await;
    assert_eq!(human.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&human.stderr).contains("Issue saved"));

    for id in ["a/../b", ".", "..", "disk ?#%/🦀"] {
        result(cli(dir.path(), &["--json", "open", id]).await, 0);
        assert_eq!(
            result(cli(dir.path(), &["--json", "get", id]).await, 0)["id"],
            id
        );
    }
    for args in [
        vec!["--json", "get", "missing"],
        vec!["--json", "close", "missing"],
        vec!["--json", "list", "--limit", "0"],
    ] {
        assert!(result(cli(dir.path(), &args).await, 1)["error"].is_string());
    }
    let human = cli(dir.path(), &["list"]).await;
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("issues (offset 0)"));

    std::fs::write(
        dir.path().join("client.yaml"),
        config.replace("shared-token", "wrong-token"),
    )
    .unwrap();
    assert!(
        result(cli(dir.path(), &["--json", "list"]).await, 1)["error"]
            .as_str()
            .unwrap()
            .contains("401")
    );
    server.abort();
    let _ = server.await;
    std::fs::write(dir.path().join("client.yaml"), config).unwrap();
    assert!(result(cli(dir.path(), &["--json", "list"]).await, 1)["error"].is_string());
}

#[tokio::test]
async fn configuration_and_argument_errors_are_clear_and_secret_free() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        result(cli(dir.path(), &["--json", "list"]).await, 1)["error"]
            .as_str()
            .unwrap()
            .contains("configuration")
    );
    std::fs::write(dir.path().join("client.yaml"), "api_token: [super-secret").unwrap();
    let error = result(cli(dir.path(), &["--json", "list"]).await, 1);
    assert!(!error.to_string().contains("super-secret"));
    assert_eq!(
        cli(dir.path(), &["list", "--unknown"]).await.status.code(),
        Some(1)
    );
    assert!(cli(dir.path(), &["--help"]).await.status.success());
    assert!(cli(dir.path(), &["--version"]).await.status.success());
}

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn serve_without_pushover_supports_cli_open_close_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    std::fs::write(
        dir.path().join("server.yaml"),
        format!("port: {port}\napi_token: token\ndatabase: data/issues.sqlite3\n"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("client.yaml"),
        format!("api_token: token\nbase_url: http://127.0.0.1:{port}\n"),
    )
    .unwrap();
    drop(reservation);
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_happer"))
            .current_dir(dir.path())
            .arg("serve")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "server exited before readiness"
            );
            if let Ok(response) = client
                .get(format!("http://127.0.0.1:{port}/v1/issues"))
                .bearer_auth("token")
                .send()
                .await
            {
                break response;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.json::<Value>().await.unwrap()["total"], 0);
    assert!(dir.path().join("data/issues.sqlite3").is_file());
    for (args, changed, count) in [
        (vec!["--json", "open", "x"], true, 1),
        (vec!["--json", "open", "x"], false, 1),
        (vec!["--json", "close", "x"], true, 1),
        (vec!["--json", "open", "x"], true, 2),
    ] {
        let output = result(cli(dir.path(), &args).await, 0);
        assert_eq!(output["notification"]["status"], "not_attempted");
        assert_eq!(output["issue"]["notification"]["status"], "not_attempted");
        assert_eq!(output["changed"], changed);
        assert_eq!(output["issue"]["opening_count"], count);
    }
    drop(child);
    let store = Store::open_file(&dir.path().join("data/issues.sqlite3")).unwrap();
    assert_eq!(
        store.get("x".into()).await.unwrap().notification,
        Notification::not_attempted()
    );
}
