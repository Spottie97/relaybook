fn main() -> std::process::ExitCode {
    cli::execute()
}

mod agents;
mod claude_hook;
mod cli;
mod commands;
mod gitinfo;
mod graphify;
mod hooks;
mod install;
mod manifest;
mod paths;
mod picker;
mod render;
mod runloop;
mod setup;
mod uninstall;
mod workflow;
