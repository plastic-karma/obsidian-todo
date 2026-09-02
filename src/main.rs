mod cli;
mod output;

fn main() {
    std::process::exit(i32::from(cli::run()));
}
