const PLANNER: &str = include_str!("../templates/guide-planner.md");
const EXECUTOR: &str = include_str!("../templates/guide-executor.md");

pub(crate) enum Role {
    Planner,
    Executor,
}

pub(crate) fn render(role: Option<Role>) -> String {
    match role {
        Some(Role::Planner) => PLANNER.to_string(),
        Some(Role::Executor) => EXECUTOR.to_string(),
        None => format!("{PLANNER}\n{EXECUTOR}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{EXECUTOR, PLANNER, Role, render};

    #[test]
    fn each_role_stays_under_four_thousand_bytes() {
        assert!(PLANNER.len() <= 4000, "{}", PLANNER.len());
        assert!(EXECUTOR.len() <= 4000, "{}", EXECUTOR.len());
        assert!(PLANNER.contains("depends-on login"));
        assert!(PLANNER.contains("planning from memory"));
        assert!(PLANNER.contains("1,500 bytes"));
        assert!(EXECUTOR.contains("task claim login"));
        assert!(EXECUTOR.contains("working unclaimed"));
        assert!(EXECUTOR.contains("reopened:"));
        let both = render(None);
        assert!(both.contains("# Planner"));
        assert!(both.contains("# Executor"));
        assert!(render(Some(Role::Planner)).starts_with("# Planner"));
        assert!(render(Some(Role::Executor)).starts_with("# Executor"));
    }
}
