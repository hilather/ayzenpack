#![forbid(unsafe_code)]

mod cli;

fn main() {
    if let Err(err) = cli::run() {
        eprintln!("ayzenpack: {err:#}");
        std::process::exit(1);
    }
}
