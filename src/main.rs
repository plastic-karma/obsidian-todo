mod cli;
mod input_ui;
mod output;

fn main() {
    std::process::exit(i32::from(cli::run()));
}
