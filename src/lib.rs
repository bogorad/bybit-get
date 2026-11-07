mod cli;
mod client;
mod config;

use cli::{parse, Action};
use client::BybitClient;
use config::load_config;
use std::io::Write;
use std::sync::{atomic::AtomicBool, Arc};

pub const USAGE: &str = "Usage: bybit-get [--all | --cards | LAST4] [--config PATH]\n\n\
  no action     Print this help without reading config or credentials.\n\
  --all         Print all available Card transaction events as a JSON array.\n\
  LAST4         Print all events for that card (exactly four ASCII digits).\n\
  --cards       Print sorted card endings found in transaction history.\n\
  --help, -h    Show this help without reading config or credentials.\n\
  --version     Show the program version.\n\n\
Authorization, reversal, financial, and refund events are included.\n\
Output is written only after every requested category has been retrieved.\n\n\
Default config: ~/.config/bybit-get/config.toml\n\
Default credentials: /run/secrets/api_keys/bybit/api_key and api_secret.\n\
Environment overrides: BYBIT_API_KEY_FILE, BYBIT_API_SECRET_FILE, BYBIT_BASE_URL.\n";

pub fn execute<I, O, E>(args: I, mut stdout: O, mut stderr: E, interrupted: Arc<AtomicBool>) -> i32
where
    I: IntoIterator<Item = std::ffi::OsString>,
    O: Write,
    E: Write,
{
    execute_with_client(args, &mut stdout, &mut stderr, |path| {
        BybitClient::from_settings(load_config(path)?, interrupted)
    })
}

fn execute_with_client<I, O, E, F>(args: I, mut stdout: O, mut stderr: E, make_client: F) -> i32
where
    I: IntoIterator<Item = std::ffi::OsString>,
    O: Write,
    E: Write,
    F: FnOnce(Option<&str>) -> Result<BybitClient, String>,
{
    let parsed = match parse(args) {
        Ok(value) => value,
        Err(message) => {
            let _ = writeln!(stderr, "bybit-get: {message}");
            return 2;
        }
    };
    match parsed.action {
        Action::Help => {
            if stdout.write_all(USAGE.as_bytes()).is_err() {
                return 1;
            }
            0
        }
        Action::Version => {
            if writeln!(stdout, "bybit-get {}", env!("CARGO_PKG_VERSION")).is_err() {
                return 1;
            }
            0
        }
        action => {
            let mut client = match make_client(parsed.config.as_deref()) {
                Ok(value) => value,
                Err(message) => {
                    let _ = writeln!(stderr, "bybit-get: {message}");
                    return 1;
                }
            };
            let filter = match &action {
                Action::Last4(value) => Some(value.as_str()),
                _ => None,
            };
            let records = match client.fetch_all(filter) {
                Ok(value) => value,
                Err(message) => {
                    let _ = writeln!(stderr, "bybit-get: {message}");
                    return 1;
                }
            };
            let result = if matches!(action, Action::Cards) {
                let mut cards: Vec<String> = records.iter().map(|r| r.last4.clone()).collect();
                cards.sort();
                cards.dedup();
                serde_json::to_vec(&cards)
            } else {
                let raws: Vec<&serde_json::value::RawValue> =
                    records.iter().map(|r| r.raw.as_ref()).collect();
                serde_json::to_vec(&raws)
            };
            match result {
                Ok(bytes) => {
                    if stdout.write_all(&bytes).is_err() || stdout.write_all(b"\n").is_err() {
                        return 1;
                    }
                    0
                }
                Err(_) => {
                    let _ = writeln!(stderr, "bybit-get: could not write JSON output");
                    1
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
