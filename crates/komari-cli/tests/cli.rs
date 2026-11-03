use std::process::Command;

use komari_cli::version;

fn bin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_komari"));
    // Isolate the tests from the developer's shell configuration.
    for key in [
        "KOMARI_LISTEN",
        "KOMARI_DB_TYPE",
        "KOMARI_DB_FILE",
        "KOMARI_DB_HOST",
        "KOMARI_DB_PORT",
        "KOMARI_DB_USER",
        "KOMARI_DB_PASS",
        "KOMARI_DB_NAME",
        "KOMARI_LOG_LEVEL",
        "KOMARI_THEME",
        "KOMARI_ALLOW_CORS",
        "KOMARI_ENABLE_CLOUDFLARED",
        "KOMARI_CLOUDFLARED_TOKEN",
        "KOMARI_WS_DISABLE_ORIGIN",
    ] {
        command.env_remove(key);
    }
    command
}

#[test]
fn help_works_for_the_root_and_every_subcommand() {
    let cases: &[&[&str]] = &[
        &["--help"],
        &["server", "--help"],
        &["chpasswd", "--help"],
        &["disable-2fa", "--help"],
        &["disable2FA", "--help"],
        &["permit-login", "--help"],
        &["permitPasswordLogin", "--help"],
    ];

    for args in cases {
        let output = bin()
            .args(*args)
            .output()
            .unwrap_or_else(|error| panic!("failed to run {args:?}: {error}"));
        assert!(
            output.status.success(),
            "{args:?} exited with {:?}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Usage:"),
            "{args:?} did not print help:\n{stdout}"
        );
    }
}

#[test]
fn version_comes_from_the_build_time_version() {
    let output = bin().arg("--version").output().expect("run --version");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(version::VERSION),
        "--version did not contain {}:\n{stdout}",
        version::VERSION
    );
}

#[test]
fn server_prints_the_resolved_startup_configuration() {
    let output = bin()
        .args(["server", "--listen", "127.0.0.1:34567"])
        .args(["--db-type", "sqlite", "--database", "test-data/test.db"])
        .args(["--log-level", "debug", "--theme", "test-theme"])
        .arg("--allow-cors")
        .output()
        .expect("run server");

    assert!(
        output.status.success(),
        "server exited with {:?}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in [
        "127.0.0.1:34567",
        "test-data/test.db",
        "test-theme",
        "allow_cors=true",
        "hash",
    ] {
        assert!(
            stderr.contains(expected),
            "startup output did not contain {expected:?}:\n{stderr}"
        );
    }
}

#[test]
fn cli_definition_is_internally_consistent() {
    use clap::CommandFactory;
    komari_cli::cli::Cli::command().debug_assert();
}
