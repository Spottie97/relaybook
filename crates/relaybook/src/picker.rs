use std::io::{IsTerminal, stdin, stdout};

pub(crate) const MODES: &[&str] = &["manual", "auto"];
pub(crate) const GATES: &[&str] = &["none", "plan", "plan,review"];

pub(crate) enum Pick {
    UseExisting,
    Selected(String),
    Cancelled,
}

pub(crate) fn current_index(options: &[&str], current: &str) -> usize {
    options
        .iter()
        .position(|item| *item == current)
        .unwrap_or(0)
}

pub(crate) fn pick(label: &str, options: &[&str], current: &str) -> Result<Pick, String> {
    if !stdin().is_terminal() || !stdout().is_terminal() {
        return Ok(Pick::UseExisting);
    }
    let choice = dialoguer::Select::new()
        .with_prompt(format!("{label} (current: {current})"))
        .items(options)
        .default(current_index(options, current))
        .interact_opt()
        .map_err(|err| err.to_string())?;
    Ok(match choice {
        Some(index) => Pick::Selected(options[index].to_string()),
        None => Pick::Cancelled,
    })
}

#[cfg(test)]
mod tests {
    use super::{GATES, MODES, current_index};

    #[test]
    fn option_lists_match_the_commands() {
        assert_eq!(MODES, ["manual", "auto"]);
        assert_eq!(GATES, ["none", "plan", "plan,review"]);
        assert_eq!(current_index(MODES, "auto"), 1);
        assert_eq!(current_index(GATES, "plan"), 1);
        assert_eq!(current_index(GATES, "missing"), 0);
    }
}
