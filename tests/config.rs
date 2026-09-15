use flare::config::{ClientConfig, ServerConfig};

fn server_yaml() -> String {
    format!(
        "api_token: shared-secret\npushover:\n  app_token: {}\n  user_key: {}\n",
        "a".repeat(30),
        "u".repeat(30)
    )
}

#[test]
fn defaults_and_relative_paths_are_config_relative() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.yaml");
    std::fs::write(&path, server_yaml()).unwrap();
    let config = ServerConfig::load(&path).unwrap();
    assert_eq!(config.database, dir.path().join("flare.sqlite3"));
    assert_eq!(config.port, 8000);
    assert_eq!(config.host.to_string(), "127.0.0.1");
    assert!(!format!("{config:?}").contains("shared-secret"));
    let path = dir.path().join("client.yaml");
    std::fs::write(&path, "api_token: shared-secret\n").unwrap();
    let config = ClientConfig::load(&path).unwrap();
    assert_eq!(config.base_url, "http://127.0.0.1:8000");
    assert_eq!(config.timeout, 15.0);
}

#[test]
fn pushover_can_be_omitted_or_null() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.yaml");
    for yaml in ["api_token: token\n", "api_token: token\npushover: null\n"] {
        std::fs::write(&path, yaml).unwrap();
        assert!(ServerConfig::load(&path).unwrap().pushover.is_none());
    }
    std::fs::write(&path, server_yaml()).unwrap();
    assert!(ServerConfig::load(&path).unwrap().pushover.is_some());
}

#[test]
fn invalid_configurations_fail_without_echoing_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.yaml");
    for text in [
        String::new(),
        "api_token: [my-secret".into(),
        "api_token: my-secret\npushover: {}\n".into(),
        "api_token: my-secret\npushover:\n  app_token: my-secret\n".into(),
        format!("{}port: 0\n", server_yaml()),
        format!("{}database: ':memory:'\n", server_yaml()),
        format!("{}unknown: my-secret\n", server_yaml()),
        server_yaml().replace(&"a".repeat(30), "my-secret"),
        server_yaml().replace("shared-secret", "''"),
        server_yaml().replace("shared-secret", "'has spaces'"),
        format!("{}  device: 'bad device'\n", server_yaml()),
    ] {
        std::fs::write(&path, text).unwrap();
        let error = ServerConfig::load(&path).unwrap_err().to_string();
        assert!(!error.contains("my-secret"));
        assert!(!error.contains("shared-secret"));
    }
    for extra in [
        "timeout: 0",
        "timeout: .nan",
        "timeout: -1",
        "timeout: 1e100",
        "base_url: ftp://localhost",
        "base_url: https://user:my-secret@example.com",
        "base_url: https://example.com?token=my-secret",
    ] {
        std::fs::write(&path, format!("api_token: shared-secret\n{extra}\n")).unwrap();
        assert!(
            !ClientConfig::load(&path)
                .unwrap_err()
                .to_string()
                .contains("my-secret")
        );
    }
    assert!(ClientConfig::load(&dir.path().join("missing")).is_err());
}

#[test]
fn delivery_and_routing_configuration_is_validated_and_secrets_are_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.yaml");
    let valid = "api_token: shared-secret\ndelivery:\n  max_attempts: 3\n  retry_base_seconds: 2\n  retry_max_seconds: 60\ndestinations:\n  audit:\n    type: webhook\n    url: https://example.com/hook?token=webhook-secret\n    bearer_token: bearer-secret\ndefault_destinations: [audit]\nroutes:\n  critical: [audit]\n  info: []\n";
    std::fs::write(&path, valid).unwrap();
    let config = ServerConfig::load(&path).unwrap();
    assert_eq!(config.delivery.max_attempts, 3);
    let debug = format!("{config:?}");
    for secret in ["shared-secret", "webhook-secret", "bearer-secret"] {
        assert!(!debug.contains(secret));
    }
    for invalid in [
        valid.replace("max_attempts: 3", "max_attempts: 0"),
        valid.replace("retry_max_seconds: 60", "retry_max_seconds: 1"),
        valid.replace("type: webhook", "type: unknown"),
        valid.replace(
            "https://example.com/hook?token=webhook-secret",
            "file:///secret",
        ),
        valid.replace(
            "default_destinations: [audit]",
            "default_destinations: [missing]",
        ),
        valid.replace("critical: [audit]", "critical: [audit, audit]"),
        valid.replace("critical:", "urgent:"),
        valid.replace("bearer-secret", "'has spaces'"),
    ] {
        std::fs::write(&path, invalid).unwrap();
        assert!(ServerConfig::load(&path).is_err());
    }
}
