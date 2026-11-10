fn main() -> std::process::ExitCode {
    cli::execute()
}

mod cli;
mod gitinfo;
mod graphify;
mod hooks;
mod install;
mod paths;
mod render;
mod setup;
mod workflow;
