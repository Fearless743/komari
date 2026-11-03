//! Command-line interface for the Komari Rust backend.

pub mod cli;
pub mod logging;
pub mod version;

use std::path::Path;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use komari_config::{Config, ConfigOverrides, DatabaseType, LogLevel};
use komari_db::Db;
use tracing::{debug, info};

use crate::cli::{ChpasswdArgs, Cli, Command, Disable2faArgs, GlobalArgs, ServerArgs};

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
        Command::Disable2fa(args) => run_disable_2fa(&cli.global, args),
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
    if !config.db.is_sqlite() {
        anyhow::bail!(
            "database type {:?} is not supported yet; only sqlite is implemented",
            config.db.db_type
        );
    }

    let server_config = komari_api::server::ServerConfig::from_app_config(&config);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to create the Tokio runtime")?;
    runtime
        .block_on(komari_api::server::run(server_config))
        .context("HTTP server failed")
}

fn run_chpasswd(global: &GlobalArgs, args: ChpasswdArgs) -> Result<()> {
    logging::init(LogLevel::Info);
    let Some(password) = args.password.filter(|password| !password.is_empty()) else {
        // Go prints the command help and returns exit code 0.
        print_chpasswd_help();
        return Ok(());
    };

    let Some(db) = open_existing_db(global)? else {
        return Ok(());
    };

    let username = match args.username {
        Some(username) => username,
        None => db
            .accounts()
            .list()
            .context("failed to list user accounts")?
            .into_iter()
            .min_by(|left, right| left.uuid.cmp(&right.uuid))
            .map(|user| user.username)
            .unwrap_or_default(),
    };

    println!("Changing password for user: {username}");
    if username.is_empty() {
        println!("Error: 无法找到用户名");
        return Ok(());
    }

    if let Err(error) = komari_auth::change_password(&db, &username, &password) {
        let message = match error {
            komari_auth::AuthError::UserNotFound(_) => "无法找到用户名".to_owned(),
            other => other.to_string(),
        };
        println!("Error: {message}");
        return Ok(());
    }
    println!("Password changed successfully, new password: {password}");

    if let Err(error) = db.sessions().delete_all() {
        println!("Unable to force logout of other devices: {error}");
        return Ok(());
    }
    println!("Please restart the server to apply the changes.");
    Ok(())
}

fn print_chpasswd_help() {
    let mut command = Cli::command();
    if let Some(subcommand) = command.find_subcommand_mut("chpasswd") {
        println!("{}", subcommand.render_help());
    }
}

fn run_disable_2fa(global: &GlobalArgs, args: Disable2faArgs) -> Result<()> {
    logging::init(LogLevel::Info);
    // Go creates and AutoMigrates the database for this command.
    let db = open_or_create_db(global)?;

    if let Some(username) = args.username {
        let user = db
            .accounts()
            .require_by_username(&username)
            .with_context(|| format!("user not found: {username}"))?;
        komari_auth::disable_2fa(&db, &user.uuid).context("failed to disable 2FA for the user")?;
    } else {
        komari_auth::disable_all_2fa(&db).context("failed to disable 2FA")?;
    }

    println!("2FA has been disabled.");
    println!("Please restart the server to apply the changes.");
    Ok(())
}

fn run_permit_password_login(global: &GlobalArgs) -> Result<()> {
    logging::init(LogLevel::Info);
    // Go creates and AutoMigrates the database for this command.
    let db = open_or_create_db(global)?;

    // Current Go reads this key/value row; the legacy command's column update
    // is only relevant to pre-1.1.4 schemas.
    db.config()
        .set("disable_password_login", &false)
        .context("failed to update disable_password_login")?;

    println!("Password login has been permitted.");
    println!("Please restart the server to apply the changes.");
    Ok(())
}

/// Open an existing database for `chpasswd`.
///
/// Mirrors Go `cmd/chpasswd.go`, which prints `Database file does not exist.`
/// and exits successfully instead of creating an empty database.
fn open_existing_db(global: &GlobalArgs) -> Result<Option<Db>> {
    let config = load_aux_db_config(global)?;
    if config.db.file != ":memory:" && !Path::new(&config.db.file).exists() {
        println!("Database file does not exist.");
        return Ok(None);
    }

    let db = Db::open(&config.db.file)
        .with_context(|| format!("failed to open database {}", config.db.file))?;
    // Go `GetDBInstance` runs AutoMigrate even for chpasswd. This also makes an
    // empty file (freshly touched SQLite db) usable instead of failing with
    // "no such table: users".
    db.migrate()
        .with_context(|| format!("failed to migrate database {}", config.db.file))?;
    Ok(Some(db))
}

/// Open or create and migrate a database for `disable-2fa`/`permit-login`,
/// matching the Go commands' `dbcore.GetDBInstance()` + `AutoMigrate` path.
fn open_or_create_db(global: &GlobalArgs) -> Result<Db> {
    let config = load_aux_db_config(global)?;
    let db = Db::open(&config.db.file)
        .with_context(|| format!("failed to open database {}", config.db.file))?;
    db.migrate()
        .with_context(|| format!("failed to migrate database {}", config.db.file))?;
    Ok(db)
}

fn load_aux_db_config(global: &GlobalArgs) -> Result<Config> {
    let config = load_aux_config(global)?;
    if !config.db.is_sqlite() {
        anyhow::bail!(
            "database type {:?} is not supported yet; only sqlite is implemented",
            config.db.db_type
        );
    }
    Ok(config)
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
