use std::path::Path;

pub(crate) const INSTALL_HINT: &str = "graphify is not on PATH. Install it with `uv tool install graphifyy` or `pipx install graphifyy`.";

pub(crate) struct CommandResult {
    pub ok: bool,
    pub detail: String,
}

pub(crate) fn installed() -> bool {
    crate::exe::Program::resolve("graphify").is_some()
}

pub(crate) fn run(root: &Path, args: &[&str]) -> CommandResult {
    let Some(program) = crate::exe::Program::resolve("graphify") else {
        return CommandResult {
            ok: false,
            detail: "graphify is not on PATH".to_string(),
        };
    };
    match program.command().args(args).current_dir(root).output() {
        Ok(output) if output.status.success() => CommandResult {
            ok: true,
            detail: summary(&output.stdout, &output.stderr, "ok"),
        },
        Ok(output) => {
            let fallback = format!("exit {}", output.status.code().unwrap_or(1));
            CommandResult {
                ok: false,
                detail: summary(&output.stderr, &output.stdout, &fallback),
            }
        }
        Err(err) => CommandResult {
            ok: false,
            detail: err.to_string(),
        },
    }
}

fn summary(primary: &[u8], secondary: &[u8], fallback: &str) -> String {
    let primary = first_line(primary);
    if !primary.is_empty() {
        return primary;
    }
    let secondary = first_line(secondary);
    if !secondary.is_empty() {
        return secondary;
    }
    fallback.to_string()
}

fn first_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().next().unwrap_or("").trim();
    let mut shortened = line.chars().take(200).collect::<String>();
    if line.chars().count() > 200 {
        shortened.push('…');
    }
    shortened
}
