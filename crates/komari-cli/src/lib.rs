//! Command-line interface for the Komari Rust backend.

pub mod cli;
pub mod logging;
pub mod version;

use anyhow::{Context, Result};
use clap::Parser;
use komari_config::{Config, ConfigOverrides, DatabaseType, LogLevel};
use tracing::{debug, info};

use crate::cli::{ChpasswdArgs, Cli, Command, GlobalArgs, ServerArgs};

/// Parse the process arguments and execute the selected command.
pub fn run() -> Result<()> {
    run_cli(Cli::parse())
}

/// Execute an already parsed CLI. Kept separate from [`run`] so it can be
/// tested without touching process arguments.
pub fn run_cli(cli: Cli) -> Result<()> {
    match cli
        .command
        .unwrap_or(Command::Server(ServerArgs::default()))
    {
        Command::Server(args) => run_server(&cli.global, args),
        Command::Chpasswd(args) => run_chpasswd(&cli.global, args),
        Command::Disable2fa => run_disable_2fa(&cli.global),
        Command::PermitLogin => run_permit_password_login(&cli.global),
    }
}

fn run_server(global: &GlobalArgs, args: ServerArgs) -> Result<()> {
    let default_log_level = version::default_log_level();
    let overrides = global.overrides(args.listen)?;
    let config = Config::load_with(overrides, default_log_level)?;

    logging::init(config.log_level);

    info!(
        version = version::VERSION,
        hash = version::VERSION_HASH,
        "Komari Monitor"
    );
    if version::is_development() {
        debug!("version hash is unknown, treating this as a development build");
    }

    info!(
        listen = %config.listen,
        db_type = %config.db.db_type,
        db_file = %config.db.file,
        db_host = %config.db.host,
        db_port = config.db.port,
        theme = %config.theme,
        allow_cors = config.allow_cors,
        log_level = %config.log_level,
        enable_cloudflared = config.enable_cloudflared,
        ws_disable_origin = config.ws_disable_origin,
        "startup configuration"
    );
    info!(
        "Starting server on {} ... (HTTP runtime is provided by a follow-up task)",
        config.listen
    );

    Ok(())
}

fn run_chpasswd(global: &GlobalArgs, args: ChpasswdArgs) -> Result<()> {
    logging::init(LogLevel::Info);
    let Some(password) = args.password else {
        anyhow::bail!("missing --password; run `komari chpasswd --help` for usage");
    };
    if password.is_empty() {
        anyhow::bail!("--password must not be empty");
    }

    let config = load_aux_config(global)?;
    println!(
        "chpasswd is not implemented in the Rust backend yet (database: {}).",
        config.db.file
    );
    println!("The password was not changed. Use the Go backend until this command lands.");
    Ok(())
}

fn run_disable_2fa(global: &GlobalArgs) -> Result<()> {
    logging::init(LogLevel::Info);
    let config = load_aux_config(global)?;
    println!(
        "disable-2fa is not implemented in the Rust backend yet (database: {}).",
        config.db.file
    );
    println!("No change was made. Use the Go backend until this command lands.");
    Ok(())
}

fn run_permit_password_login(global: &GlobalArgs) -> Result<()> {
    logging::init(LogLevel::Info);
    let config = load_aux_config(global)?;
    println!(
        "permit-login is not implemented in the Rust backend yet (database: {}).",
        config.db.file
    );
    println!("No change was made. Use the Go backend until this command lands.");
    Ok(())
}

fn load_aux_config(global: &GlobalArgs) -> Result<Config> {
    Config::load_with(global.overrides(None)?, LogLevel::Info)
        .context("failed to load Komari configuration")
}

impl GlobalArgs {
    fn overrides(&self, listen: Option<String>) -> Result<ConfigOverrides> {
        let db_type = self
            .db_type
            .as_deref()
            .map(str::parse::<DatabaseType>)
            .transpose()
            .context("invalid --db-type")?;
        let log_level = self
            .log_level
            .as_deref()
            .map(str::parse::<LogLevel>)
            .transpose()
            .context("invalid --log-level")?;

        Ok(ConfigOverrides {
            listen,
            db_type,
            db_file: self.database.clone(),
            db_host: self.db_host.clone(),
            db_port: self.db_port,
            db_user: self.db_user.clone(),
            db_password: self.db_pass.clone(),
            db_name: self.db_name.clone(),
            log_level,
            theme: self.theme.clone(),
            allow_cors: self.allow_cors,
            enable_cloudflared: self.enable_cloudflared,
            ws_disable_origin: self.ws_disable_origin,
        })
    }
}
