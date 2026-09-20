use flares::config::{ClientConfig, ServerConfig};

fn server_yaml() -> String {
    format!(
        "server:\n  api_token: shared-secret\ndestinations:\n  pushover:\n    type: pushover\n    app_token: {}\n    user_key: {}\n",
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
    assert_eq!(config.database, dir.path().join("flares.sqlite3"));
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
fn destinations_can_be_omitted_or_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.yaml");
    for yaml in [
        "server: {api_token: token}\n",
        "server: {api_token: token}\ndestinations: {}\n",
    ] {
        std::fs::write(&path, yaml).unwrap();
        assert!(ServerConfig::load(&path).unwrap().destinations.is_empty());
    }
    std::fs::write(&path, server_yaml()).unwrap();
    assert!(
        ServerConfig::load(&path)
            .unwrap()
            .destinations
            .contains_key("pushover")
    );
}

#[test]
fn invalid_configurations_fail_without_echoing_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.yaml");
    for text in [
        String::new(),
        "server: {api_token: [my-secret".into(),
        "server: {api_token: my-secret}\ndestinations: {phone: {type: pushover}}\n".into(),
        "server: {api_token: my-secret}\ndestinations: {phone: {type: pushover, app_token: my-secret}}\n".into(),
        server_yaml().replace("server:\n", "server:\n  listen: 127.0.0.1:0\n"),
        format!("{}storage: {{database: ':memory:'}}\n", server_yaml()),
        format!("{}unknown: my-secret\n", server_yaml()),
        server_yaml().replace(&"a".repeat(30), "my-secret"),
        server_yaml().replace("shared-secret", "''"),
        server_yaml().replace("shared-secret", "'has spaces'"),
        format!("{}    device: 'bad device'\n", server_yaml()),
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
    let valid = "server: {api_token: shared-secret}\ndelivery:\n  retry: {max_attempts: 3, base_delay: 2s, max_delay: 60s}\ndestinations:\n  audit:\n    type: webhook\n    url: https://example.com/hook?token=webhook-secret\n    bearer_token: bearer-secret\nrouting:\n  default: [audit]\n  severity:\n    critical: [audit]\n    info: []\n";
    std::fs::write(&path, valid).unwrap();
    let config = ServerConfig::load(&path).unwrap();
    assert_eq!(config.delivery.max_attempts, 3);
    let debug = format!("{config:?}");
    for secret in ["shared-secret", "webhook-secret", "bearer-secret"] {
        assert!(!debug.contains(secret));
    }
    for invalid in [
        valid.replace("max_attempts: 3", "max_attempts: 0"),
        valid.replace("max_delay: 60s", "max_delay: 1s"),
        valid.replace("type: webhook", "type: unknown"),
        valid.replace(
            "https://example.com/hook?token=webhook-secret",
            "file:///secret",
        ),
        valid.replace("default: [audit]", "default: [missing]"),
        valid.replace("critical: [audit]", "critical: [audit, audit]"),
        valid.replace("critical:", "urgent:"),
        valid.replace("bearer-secret", "'has spaces'"),
    ] {
        std::fs::write(&path, invalid).unwrap();
        assert!(ServerConfig::load(&path).is_err());
    }
}

#[test]
fn modern_config_resolves_secrets_and_has_no_storage_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("token"), "file-secret\n").unwrap();
    let path = dir.path().join("server.yaml");
    std::fs::write(
        &path,
        format!(
            r#"
server:
  listen: '[::1]:9000'
  api_token: {{file: token}}
storage:
  database: missing/data.sqlite3
  retention:
    deliveries: 30d
    idempotency_keys: 90d
delivery:
  retry: {{max_attempts: 3, base_delay: 10s, max_delay: 1h}}
  rate_limit: {{attempts: 20, window: 5m}}
  group_window: 2m
  queue_limit: 40
destinations:
  phone:
    type: pushover
    app_token: '{}'
    user_key: '{}'
    delivery:
      retry: {{max_attempts: 2}}
  audit:
    type: webhook
    url: https://example.com/hook?token=url-secret
    bearer_token: {{file: token}}
    delivery:
      retry: {{base_delay: 30s}}
      rate_limit: {{attempts: 4, window: 1h}}
routing:
  default: [audit]
  severity:
    critical: [phone, audit]
    info: []
"#,
            "a".repeat(30),
            "u".repeat(30)
        ),
    )
    .unwrap();
    let config = ServerConfig::load(&path).unwrap();
    assert_eq!(config.port, 9000);
    assert_eq!(config.host.to_string(), "::1");
    assert_eq!(config.api_token.expose(), "file-secret");
    assert_eq!(config.database, dir.path().join("missing/data.sqlite3"));
    assert!(!dir.path().join("missing").exists());
    assert_eq!(config.delivery.rate_window_seconds, 300);
    assert_eq!(config.delivery.group_window_seconds, 120);
    assert_eq!(config.delivery.queue_limit, 40);
    assert_eq!(config.retention.deliveries, Some(30 * 86400));
    assert_eq!(config.destination_policies["phone"].max_attempts, 2);
    assert_eq!(config.destination_policies["phone"].retry_base_seconds, 10);
    assert_eq!(config.destination_policies["audit"].retry_base_seconds, 30);
    assert_eq!(config.destination_policies["audit"].max_attempts, 3);
    let effective = config.effective();
    assert_eq!(
        effective["routing"]["severity"]["info"],
        serde_json::json!([])
    );
    assert_eq!(
        effective["destinations"]["phone"]["delivery"]["retry"]["base_delay"],
        "10s"
    );
    for value in [effective.to_string(), format!("{config:?}")] {
        for secret in [
            "file-secret",
            "url-secret",
            &"a".repeat(30),
            &"u".repeat(30),
        ] {
            assert!(!value.contains(secret));
        }
    }
}

#[test]
fn legacy_config_fields_are_rejected_even_alongside_modern_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.yaml");
    for legacy in [
        "host: 127.0.0.1",
        "port: 8000",
        "api_token: old-secret",
        "database: old.sqlite3",
        "pushover: null",
        "default_destinations: []",
        "routes: {}",
        "delivery: {max_attempts: 3}",
        "delivery: {retry_base_seconds: 10}",
        "delivery: {retry_max_seconds: 60}",
        "delivery: {rate_limit_per_minute: 120}",
        "delivery: {group_window_seconds: 30}",
        "destinations: {phone: {type: pushover, config: {app_token: old-secret, user_key: old-secret}}}",
        "delivery: {group_window: 30}",
        "delivery: {retry: {base_delay: 10}}",
        "delivery: {rate_limit: {attempts: 10, window: 60}}",
        "storage: {retention: {deliveries: 30}}",
    ] {
        for text in [
            legacy.to_owned(),
            format!("server: {{api_token: token}}\n{legacy}\n"),
        ] {
            std::fs::write(&path, text).unwrap();
            let error = ServerConfig::load(&path).unwrap_err().to_string();
            assert!(!error.contains("old-secret"));
        }
    }
    for routing in ["", "routing: {default: []}"] {
        std::fs::write(&path, format!("{}{routing}\n", server_yaml())).unwrap();
        assert!(
            ServerConfig::load(&path)
                .unwrap()
                .default_destinations
                .is_empty()
        );
    }
}

#[test]
fn config_errors_identify_fields_without_printing_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.yaml");
    for (text, field) in [
        (
            "server:\n  api_token: token\n  listen: '127.0.0.1:0'\n",
            "server.listen",
        ),
        (
            "server:\n  api_token: token\ndelivery:\n  retry: {base_delay: 1h, max_delay: 10s}\n",
            "delivery.retry.max_delay",
        ),
        (
            "server:\n  api_token: token\ndelivery:\n  rate_limit: {attempts: 5, window: 0s}\n",
            "delivery.rate_limit.window",
        ),
        (
            "server:\n  api_token: token\ndelivery:\n  retry:\n    max_attempts: secret-value\n",
            "delivery.retry.max_attempts",
        ),
        (
            "server:\n  api_token: token\ndelivery:\n  group_window: 999999999999999999999999999d\n",
            "delivery.group_window",
        ),
        (
            "server:\n  api_token: {file: absent-secret-file}\n",
            "server.api_token",
        ),
        (
            "server:\n  api_token: token\napi_token: conflicting-secret\n",
            "configuration",
        ),
        (
            "server:\n  api_token: token\ndelivery:\n  max_attempts: 2\n  retry: {max_attempts: 3}\n",
            "delivery",
        ),
        (
            "server:\n  api_token: token\nrouting:\n  default: [missing-secret]\n",
            "routing.default",
        ),
    ] {
        std::fs::write(&path, text).unwrap();
        let message = ServerConfig::load(&path).unwrap_err().to_string();
        assert!(message.contains(field), "{message}");
        for value in [
            "secret-value",
            "absent-secret-file",
            "conflicting-secret",
            "missing-secret",
        ] {
            assert!(!message.contains(value), "{message}");
        }
        if text.contains("max_attempts: secret-value") {
            assert!(message.contains("line 5"), "{message}");
        }
    }
}

#[test]
fn client_supports_duration_strings_and_secret_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("token"), "client-secret\r\n").unwrap();
    let path = dir.path().join("client.yaml");
    std::fs::write(&path, "api_token: {file: token}\ntimeout: 2m\n").unwrap();
    let config = ClientConfig::load(&path).unwrap();
    assert_eq!(config.api_token.expose(), "client-secret");
    assert_eq!(config.timeout, 120.0);
    assert!(!config.effective().to_string().contains("client-secret"));
    for timeout in ["15", "0.5", "\"15\""] {
        std::fs::write(&path, format!("api_token: token\ntimeout: {timeout}\n")).unwrap();
        assert!(ClientConfig::load(&path).is_err());
    }
}
