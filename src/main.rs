#![deny(clippy::all, clippy::pedantic, warnings)]

mod format;
mod run;
mod state;

use {
    crate::{format::CodeStr, run::run},
    atty::Stream,
    byte_unit::Byte,
    chrono::Local,
    clap::{App, AppSettings, Arg},
    env_logger::{fmt::Color, Builder},
    log::{Level, LevelFilter},
    regex::RegexSet,
    std::{
        env,
        io::{self, Write},
        process::exit,
        str::FromStr,
        thread::sleep,
        time::Duration,
    },
};

#[macro_use]
extern crate log;

// The program version
const VERSION: &str = env!("CARGO_PKG_VERSION");

// Defaults
const DEFAULT_LOG_LEVEL: LevelFilter = LevelFilter::Debug;
const DEFAULT_THRESHOLD: &str = "10 GB";

// Command-line argument and option names
const THRESHOLD_OPTION: &str = "threshold";
const KEEP_OPTION: &str = "keep";

// Size threshold argument, absolute or relative to filesystem size
#[derive(Copy, Clone)]
enum Threshold {
    Absolute(Byte),
    Percentage(f64),
}

// This struct represents the command-line arguments.
pub struct Settings {
    threshold: Threshold,
    keep: Option<RegexSet>,
}

// Set up the logger.
fn set_up_logging() {
    Builder::new()
        .filter_module(
            module_path!(),
            LevelFilter::from_str(
                &env::var("LOG_LEVEL").unwrap_or_else(|_| DEFAULT_LOG_LEVEL.to_string()),
            )
            .unwrap_or(DEFAULT_LOG_LEVEL),
        )
        .format(|buf, record| {
            let mut style = buf.style();
            style.set_bold(true);
            match record.level() {
                Level::Error => {
                    style.set_color(Color::Red);
                }
                Level::Warn => {
                    style.set_color(Color::Yellow);
                }
                Level::Info => {
                    style.set_color(Color::Green);
                }
                Level::Debug => {
                    style.set_color(Color::Blue);
                }
                Level::Trace => {
                    style.set_color(Color::Cyan);
                }
            }

            writeln!(
                buf,
                "{} {}",
                style.value(format!(
                    "[{} {}]",
                    Local::now().format("%Y-%m-%d %H:%M:%S %:z"),
                    record.level(),
                )),
                record.args(),
            )
        })
        .init();
}

// Parse the command-line arguments.
#[allow(clippy::map_err_ignore)]
fn settings() -> io::Result<Settings> {
    // Set up the command-line interface.
    let matches = App::new("Docuum")
        .version(VERSION)
        .version_short("v")
        .author("Stephan Boyer <stephan@stephanboyer.com>")
        .about("Docuum performs LRU cache eviction for Docker images.")
        .setting(AppSettings::ColoredHelp)
        .setting(AppSettings::NextLineHelp)
        .setting(AppSettings::UnifiedHelpMessage)
        .arg(
            Arg::with_name(THRESHOLD_OPTION)
                .value_name("THRESHOLD")
                .short("t")
                .long(THRESHOLD_OPTION)
                .help(&format!(
                    "Sets the maximum amount of space to be used for Docker images (default: {})",
                    DEFAULT_THRESHOLD.code_str(),
                )),
        )
        .arg(
            Arg::with_name(KEEP_OPTION)
                .value_name("REGEX")
                .short("k")
                .long(KEEP_OPTION)
                .multiple(true)
                .number_of_values(1)
                .help("Prevents deletion of images for which repository:tag matches <REGEX>"),
        )
        .get_matches();

    // Read the threshold.
    let default_threshold = Threshold::Absolute(
        Byte::from_str(DEFAULT_THRESHOLD).unwrap(), /*  Manually verified safe */
    );
    let threshold = matches.value_of(THRESHOLD_OPTION).map_or_else(
        || Ok(default_threshold),
        |threshold| match threshold.strip_suffix('%') {
            Some(threshold_percentage_string) => {
                // Threshold parameter has "%" suffix: Try parsing as f64
                threshold_percentage_string
                    .trim()
                    .parse::<f64>()
                    .map_err(|parse_error| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!(
                                "Invalid relative threshold {}. Error: {}",
                                threshold.code_str(),
                                parse_error,
                            ),
                        )
                    })
                    .map(|f| Threshold::Percentage(f / 100.0))
            }
            None => Byte::from_str(threshold)
                // Threshold parameter does not have "%" suffix: Try parsing as Byte
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Invalid threshold {}.", threshold.code_str()),
                    )
                })
                .map(Threshold::Absolute),
        },
    )?;

    let keep = match matches.values_of(KEEP_OPTION) {
        Some(values) => match RegexSet::new(values) {
            Ok(set) => Some(set),
            Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidInput, e)),
        },
        None => None,
    };

    Ok(Settings { threshold, keep })
}

// Let the fun begin!
fn main() {
    // Determine whether to print colored output.
    colored::control::set_override(atty::is(Stream::Stderr));

    // Set up the logger.
    set_up_logging();

    // Parse the command-line arguments.
    let settings = match settings() {
        Ok(settings) => settings,
        Err(error) => {
            error!("{}", error);
            exit(1);
        }
    };

    // Try to load the state from disk.
    let (mut state, mut first_run) = state::load().map_or_else(
        |error| {
            // We couldn't load any state from disk. Log the error.
            warn!(
                "Unable to load state from disk. Proceeding with initial state. Details: {}",
                error.to_string().code_str(),
            );

            // Start with the initial state.
            (state::initial(), true)
        },
        |state| (state, false),
    );

    // Stream Docker events and vacuum when necessary. Restart if an error occurs.
    loop {
        if let Err(e) = run(&settings, &mut state, &mut first_run) {
            error!("{}", e);
            info!("Retrying in 5 seconds\u{2026}");
            sleep(Duration::from_secs(5));
        }
    }
}
