use std::env;
use orchestrate_lib::cli;

fn main() {
    let args: Vec<String> = env::args().collect();
    cli::run("cargo orchestrate", &cli::strip_cargo_subcommand("orchestrate", &args));
}
