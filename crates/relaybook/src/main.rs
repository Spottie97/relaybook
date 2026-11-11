fn main() -> std::process::ExitCode {
    cli::execute()
}

mod agents;
mod claude_hook;
mod cli;
mod commands;
mod exe;
mod gitinfo;
mod graphify;
mod guide;
mod hooks;
mod install;
mod live;
mod managed;
mod manifest;
mod paths;
mod picker;
mod render;
mod runloop;
mod setup;
mod stream;
mod uninstall;
mod workflow;
