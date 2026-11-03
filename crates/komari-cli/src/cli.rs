//! Command and argument definitions.

use clap::{Args, Parser, Subcommand};

use crate::version;

#[derive(Debug, Parser)]
#[command(
    name = "komari",
    bin_name = "komari",
    version = version::VERSION,
    about = "Komari is a simple server monitoring tool.",
    long_about = "Komari is a simple server monitoring tool.\nMade by Akizon77 with love.",
    arg_required_else_help = false,
    disable_help_subcommand = true,
    propagate_version = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Persistent flags shared by every subcommand. They mirror the Go backend's
/// root flags and always take precedence over environment variables.
#[derive(Debug, Clone, Default, Args)]
pub struct GlobalArgs {
    #[arg(
        short = 't',
        long = "db-type",
        global = true,
        value_name = "DB_TYPE",
        help = "Database type (sqlite, mysql) [env: KOMARI_DB_TYPE]"
    )]
    pub db_type: Option<String>,

    #[arg(
        short = 'd',
        long = "database",
        visible_alias = "db-file",
        global = true,
        value_name = "FILE",
        help = "SQLite database file path [env: KOMARI_DB_FILE]"
    )]
    pub database: Option<String>,

    #[arg(
        long = "db-host",
        global = true,
        value_name = "HOST",
        help = "MySQL/Other database host address [env: KOMARI_DB_HOST]"
    )]
    pub db_host: Option<String>,

    #[arg(
        long = "db-port",
        global = true,
        value_name = "PORT",
        help = "MySQL/Other database port [env: KOMARI_DB_PORT]"
    )]
    pub db_port: Option<u16>,

    #[arg(
        long = "db-user",
        global = true,
        value_name = "USER",
        help = "MySQL/Other database username [env: KOMARI_DB_USER]"
    )]
    pub db_user: Option<String>,

    #[arg(
        long = "db-pass",
        global = true,
        value_name = "PASSWORD",
        help = "MySQL/Other database password [env: KOMARI_DB_PASS]"
    )]
    pub db_pass: Option<String>,

    #[arg(
        long = "db-name",
        global = true,
        value_name = "NAME",
        help = "MySQL/Other database name [env: KOMARI_DB_NAME]"
    )]
    pub db_name: Option<String>,

    #[arg(
        long = "log-level",
        global = true,
        value_name = "LEVEL",
        help = "Log level: trace, debug, info, warn, error, off [env: KOMARI_LOG_LEVEL]"
    )]
    pub log_level: Option<String>,

    #[arg(
        long = "theme",
        global = true,
        value_name = "THEME",
        help = "Theme name [env: KOMARI_THEME]"
    )]
    pub theme: Option<String>,

    #[arg(
        long = "allow-cors",
        global = true,
        value_name = "BOOL",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true",
        help = "Allow cross-origin requests [env: KOMARI_ALLOW_CORS]"
    )]
    pub allow_cors: Option<bool>,

    #[arg(
        long = "enable-cloudflared",
        global = true,
        value_name = "BOOL",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true",
        help = "Enable the cloudflared tunnel [env: KOMARI_ENABLE_CLOUDFLARED]"
    )]
    pub enable_cloudflared: Option<bool>,

    #[arg(
        long = "ws-disable-origin",
        global = true,
        value_name = "BOOL",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true",
        help = "Disable WebSocket origin checks [env: KOMARI_WS_DISABLE_ORIGIN]"
    )]
    pub ws_disable_origin: Option<bool>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the server
    Server(ServerArgs),

    /// Force change password
    Chpasswd(ChpasswdArgs),

    /// Force disable 2FA
    #[command(name = "disable-2fa", visible_alias = "disable2FA")]
    Disable2fa(Disable2faArgs),

    /// Force permit password login
    #[command(name = "permit-login", visible_alias = "permitPasswordLogin")]
    PermitLogin,
}

#[derive(Debug, Clone, Default, Args)]
pub struct ServerArgs {
    #[arg(
        short = 'l',
        long = "listen",
        value_name = "ADDR",
        help = "Listen address [env: KOMARI_LISTEN]"
    )]
    pub listen: Option<String>,
}

#[derive(Debug, Clone, Default, Args)]
pub struct ChpasswdArgs {
    #[arg(
        short = 'u',
        long = "username",
        value_name = "USERNAME",
        help = "User whose password to change (defaults to the first account)"
    )]
    pub username: Option<String>,

    #[arg(
        short = 'p',
        long = "password",
        value_name = "PASSWORD",
        help = "New password"
    )]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Default, Args)]
pub struct Disable2faArgs {
    #[arg(
        short = 'u',
        long = "username",
        value_name = "USERNAME",
        help = "Only disable 2FA for this user (defaults to all users)"
    )]
    pub username: Option<String>,
}
