fn main() -> std::process::ExitCode {
    cli::execute()
}

mod cli;
mod gitinfo;
mod install;
mod paths;
mod render;
