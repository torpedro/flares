use flares_client::{ApiClient, config::ClientConfig};
use serde_json::Value;

#[test]
fn shared_configuration_cases() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/client-config.json")).unwrap();
    let root = tempfile::tempdir().unwrap();
    let configs = root.path().join("configs");
    std::fs::create_dir(&configs).unwrap();
    for directory in [root.path(), configs.as_path()] {
        std::fs::write(directory.join("token"), "test-token\r\n").unwrap();
    }
    let path = configs.join("client.yaml");
    for case in cases {
        std::fs::write(&path, case["yaml"].as_str().unwrap()).unwrap();
        let result = ClientConfig::load(&path);
        if let Some(expected) = case.get("expected") {
            let config = result.unwrap_or_else(|e| panic!("{}: {e}", case["id"]));
            assert_eq!(
                config.api_token.expose(),
                expected["token"].as_str().unwrap(),
                "{}",
                case["id"]
            );
            assert_eq!(config.base_url, expected["base_url"].as_str().unwrap());
            assert_eq!(config.timeout, expected["timeout"].as_f64().unwrap());
            assert!(!format!("{config:?}").contains("test-token"));
            assert!(ApiClient::from_config(&path).is_ok());
        } else {
            let error = result.expect_err(case["id"].as_str().unwrap()).to_string();
            assert!(error.contains(&path.display().to_string()), "{error}");
            assert!(!error.contains("sensitive"), "{error}");
            assert!(ApiClient::from_config(&path).is_err());
        }
    }
}

#[test]
fn environment_and_default_discovery_in_subprocess() {
    let root = tempfile::tempdir().unwrap();
    let config_dir = root.path().join("flares");
    std::fs::create_dir(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("client.yaml"),
        "api_token: {env: FLARES_CONFIG_TEST_TOKEN}\n",
    )
    .unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "config_environment_child", "--nocapture"])
        .env("FLARES_CONFIG_CHILD", "1")
        .env("FLARES_CONFIG_TEST_TOKEN", "environment-secret")
        .env("XDG_CONFIG_HOME", root.path())
        .env_remove("HOME")
        .current_dir(root.path())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn config_environment_child() {
    if std::env::var_os("FLARES_CONFIG_CHILD").is_none() {
        return;
    }
    let path = flares_client::config::default_path("client.yaml").unwrap();
    let config = ClientConfig::load(&path).unwrap();
    assert_eq!(config.api_token.expose(), "environment-secret");
    assert!(ApiClient::from_default_config().is_ok());
}

#[cfg(unix)]
#[test]
fn symlinked_config_uses_target_directory_for_secrets() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(root.path().join("token"), "invalid token").unwrap();
    std::fs::write(target.join("token"), "target-token\n").unwrap();
    std::fs::write(target.join("client.yaml"), "api_token: {file: token}\n").unwrap();
    let link = root.path().join("client.yaml");
    std::os::unix::fs::symlink(target.join("client.yaml"), &link).unwrap();
    assert_eq!(
        ClientConfig::load(&link).unwrap().api_token.expose(),
        "target-token"
    );
}
