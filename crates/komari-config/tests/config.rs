use std::collections::HashMap;

use komari_config::{
    Config, ConfigError, ConfigOverrides, DatabaseType, LogLevel, SiteConfig,
    config::{DEFAULT_DB_FILE, DEFAULT_LISTEN},
    keys,
};

fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let values: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    move |key| values.get(key).cloned()
}

fn load(pairs: &[(&str, &str)], overrides: &ConfigOverrides) -> Result<Config, ConfigError> {
    Config::from_sources(lookup(pairs), overrides, LogLevel::Info)
}

#[test]
fn defaults_match_the_go_backend() {
    let config = load(&[], &ConfigOverrides::default()).expect("defaults must load");

    assert_eq!(config, Config::default());
    assert_eq!(config.listen, DEFAULT_LISTEN);
    assert_eq!(config.listen, "0.0.0.0:25774");
    assert_eq!(config.db.db_type, DatabaseType::Sqlite);
    assert_eq!(config.db.file, DEFAULT_DB_FILE);
    assert_eq!(config.db.file, "data/komari.db");
    assert_eq!(config.db.host, "localhost");
    assert_eq!(config.db.port, 3306);
    assert_eq!(config.db.user, "root");
    assert_eq!(config.db.name, "komari");
    assert_eq!(config.log_level, LogLevel::Info);
    assert_eq!(config.theme, "default");
    assert!(!config.allow_cors);
    assert!(!config.enable_cloudflared);
    assert!(!config.ws_disable_origin);
    assert_eq!(config.body_limit, None);
}

#[test]
fn environment_overrides_defaults() {
    let config = load(
        &[
            (keys::ENV_LISTEN, "127.0.0.1:8080"),
            (keys::ENV_DB_TYPE, "mysql"),
            (keys::ENV_DB_FILE, "/tmp/komari.db"),
            (keys::ENV_DB_HOST, "db.internal"),
            (keys::ENV_DB_PORT, "3307"),
            (keys::ENV_DB_USER, "komari"),
            (keys::ENV_DB_PASS, "secret"),
            (keys::ENV_DB_NAME, "komari_test"),
            (keys::ENV_LOG_LEVEL, "warn"),
            (keys::ENV_THEME, "dark"),
            (keys::ENV_ALLOW_CORS, "true"),
            (keys::ENV_ENABLE_CLOUDFLARED, "1"),
            (keys::ENV_CLOUDFLARED_TOKEN, "token"),
            (keys::ENV_WS_DISABLE_ORIGIN, "TRUE"),
            (keys::ENV_BODY_LIMIT, "1048576"),
        ],
        &ConfigOverrides::default(),
    )
    .expect("environment configuration must load");

    assert_eq!(config.listen, "127.0.0.1:8080");
    assert_eq!(config.db.db_type, DatabaseType::Mysql);
    assert_eq!(config.db.file, "/tmp/komari.db");
    assert_eq!(config.db.host, "db.internal");
    assert_eq!(config.db.port, 3307);
    assert_eq!(config.db.user, "komari");
    assert_eq!(config.db.password, "secret");
    assert_eq!(config.db.name, "komari_test");
    assert_eq!(config.log_level, LogLevel::Warn);
    assert_eq!(config.theme, "dark");
    assert!(config.allow_cors);
    assert!(config.enable_cloudflared);
    assert_eq!(config.cloudflared_token.as_deref(), Some("token"));
    assert!(config.ws_disable_origin);
    assert_eq!(config.body_limit, Some(1_048_576));
}

#[test]
fn flags_override_environment() {
    let overrides = ConfigOverrides {
        listen: Some("0.0.0.0:9999".to_owned()),
        db_type: Some(DatabaseType::Sqlite),
        db_file: Some("/srv/flag.db".to_owned()),
        db_port: Some(4000),
        log_level: Some(LogLevel::Error),
        theme: Some("flag-theme".to_owned()),
        allow_cors: Some(false),
        ..ConfigOverrides::default()
    };

    let config = load(
        &[
            (keys::ENV_LISTEN, "127.0.0.1:1111"),
            (keys::ENV_DB_TYPE, "mysql"),
            (keys::ENV_DB_FILE, "/tmp/env.db"),
            (keys::ENV_DB_PORT, "3307"),
            (keys::ENV_DB_NAME, "env_name"),
            (keys::ENV_LOG_LEVEL, "debug"),
            (keys::ENV_THEME, "env-theme"),
            (keys::ENV_ALLOW_CORS, "true"),
        ],
        &overrides,
    )
    .expect("flags must take precedence");

    assert_eq!(config.listen, "0.0.0.0:9999");
    assert_eq!(config.db.db_type, DatabaseType::Sqlite);
    assert_eq!(config.db.file, "/srv/flag.db");
    assert_eq!(config.db.port, 4000);
    assert_eq!(config.log_level, LogLevel::Error);
    assert_eq!(config.theme, "flag-theme");
    assert!(!config.allow_cors);
    // Values only present in the environment still apply.
    assert_eq!(config.db.name, "env_name");
}

#[test]
fn empty_environment_values_fall_through_to_defaults() {
    let config = load(
        &[
            (keys::ENV_LISTEN, ""),
            (keys::ENV_DB_TYPE, ""),
            (keys::ENV_DB_FILE, ""),
            (keys::ENV_DB_PORT, ""),
            (keys::ENV_LOG_LEVEL, ""),
            (keys::ENV_THEME, ""),
            (keys::ENV_ALLOW_CORS, ""),
        ],
        &ConfigOverrides::default(),
    )
    .expect("empty values behave like unset variables");

    assert_eq!(config, Config::default());
}

#[test]
fn invalid_environment_values_are_rejected() {
    for (key, value) in [
        (keys::ENV_DB_TYPE, "oracle"),
        (keys::ENV_DB_PORT, "not-a-port"),
        (keys::ENV_DB_PORT, "70000"),
        (keys::ENV_LOG_LEVEL, "verbose"),
        (keys::ENV_ALLOW_CORS, "maybe"),
    ] {
        let error = load(&[(key, value)], &ConfigOverrides::default())
            .expect_err("invalid environment value must fail");
        assert!(
            matches!(error, ConfigError::InvalidValue { .. }),
            "{key}={value} produced {error:?}"
        );
    }
}

#[test]
fn site_config_defaults_match_the_go_legacy_struct() {
    let site = SiteConfig::default();

    assert_eq!(site.sitename, "Komari");
    assert_eq!(site.description, "A simple server monitor tool.");
    assert_eq!(site.theme, "default");
    assert_eq!(site.geo_ip_provider, "ipinfo");
    assert_eq!(site.ddns_sync_interval, 10);
    assert_eq!(site.o_auth_provider, "github");
    assert_eq!(site.expire_notification_lead_days, 7);
    assert_eq!(site.traffic_limit_percentage, 80.0);
    assert_eq!(site.record_preserve_time, 720);
}

#[test]
fn rejects_a_non_positive_body_limit() {
    let error = load(&[(keys::ENV_BODY_LIMIT, "0")], &ConfigOverrides::default())
        .expect_err("zero body limit must be rejected");
    assert!(matches!(error, ConfigError::InvalidValue { .. }));
}

#[test]
fn ws_disable_origin_env_only_accepts_case_insensitive_true() {
    for value in ["true", "TRUE", "True", "tRuE"] {
        let config = load(
            &[(keys::ENV_WS_DISABLE_ORIGIN, value)],
            &ConfigOverrides::default(),
        )
        .expect("true values must load");
        assert!(
            config.ws_disable_origin,
            "{value:?} should disable the check"
        );
    }

    for value in [
        "1", "0", "yes", "on", "no", "off", "false", " true ", "true ", "",
    ] {
        let config = load(
            &[(keys::ENV_WS_DISABLE_ORIGIN, value)],
            &ConfigOverrides::default(),
        )
        .unwrap_or_else(|error| panic!("{value:?} must not be a config error: {error}"));
        assert!(
            !config.ws_disable_origin,
            "{value:?} must not disable the check"
        );
    }
}

#[test]
fn ws_disable_origin_flag_overrides_environment() {
    let overrides = ConfigOverrides {
        ws_disable_origin: Some(false),
        ..ConfigOverrides::default()
    };
    let config = load(&[(keys::ENV_WS_DISABLE_ORIGIN, "true")], &overrides).expect("flag must win");
    assert!(!config.ws_disable_origin);

    let overrides = ConfigOverrides {
        ws_disable_origin: Some(true),
        ..ConfigOverrides::default()
    };
    let config = load(&[(keys::ENV_WS_DISABLE_ORIGIN, "yes")], &overrides).expect("flag must win");
    assert!(config.ws_disable_origin);
}

#[test]
fn allow_cors_keeps_the_generic_boolean_parser() {
    // Only the WS variable follows Go's strict `EqualFold(..., "true")`;
    // the other startup booleans keep accepting 1/yes/on.
    for value in ["1", "yes", "on", "TRUE"] {
        let config = load(
            &[(keys::ENV_ALLOW_CORS, value)],
            &ConfigOverrides::default(),
        )
        .expect("generic booleans must load");
        assert!(config.allow_cors, "{value:?} should enable CORS");
    }
}
