//! Tracing initialisation.

use std::io::IsTerminal;

use komari_config::LogLevel;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Configure the global tracing subscriber. Calling this more than once is
/// harmless; only the first call installs the subscriber.
pub fn init(level: LogLevel) {
    let filter = match level {
        LogLevel::Trace => tracing::level_filters::LevelFilter::TRACE,
        LogLevel::Debug => tracing::level_filters::LevelFilter::DEBUG,
        LogLevel::Info => tracing::level_filters::LevelFilter::INFO,
        LogLevel::Warn => tracing::level_filters::LevelFilter::WARN,
        LogLevel::Error => tracing::level_filters::LevelFilter::ERROR,
        LogLevel::Off => tracing::level_filters::LevelFilter::OFF,
    };

    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_ansi(std::io::stderr().is_terminal())
                .with_writer(std::io::stderr),
        )
        .with(filter)
        .try_init();
}
