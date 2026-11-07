use std::io;
use std::sync::{atomic::AtomicBool, Arc};

fn main() {
    let interrupted = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&interrupted));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&interrupted));
    let code = bybit_get::execute(
        std::env::args_os().skip(1),
        io::stdout(),
        io::stderr(),
        interrupted,
    );
    std::process::exit(code);
}
