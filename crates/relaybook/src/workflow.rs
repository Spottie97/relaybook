use std::path::Path;
use std::process::Command;

const WORKFLOW: &str = include_str!("../templates/graphify-update.yml");
pub(crate) const TEMPLATE: &str = WORKFLOW;
const CI_REQUIREMENTS: &str = "Enable Settings > Actions > General > Allow GitHub Actions to create and approve pull requests. PRs opened with GITHUB_TOKEN do not run other workflows; set secret GRAPHIFY_PR_TOKEN (contents and pull-requests write) so required checks run.";

pub(crate) struct WriteResult {
    pub status: &'static str,
    pub detail: String,
}

pub(crate) fn install_workflow(root: &Path) -> Result<WriteResult, std::io::Error> {
    let path = root
        .join(".github")
        .join("workflows")
        .join("graphify-update.yml");
    if path.exists() {
        return Ok(WriteResult {
            status: "skipped",
            detail: with_requirements("workflow already exists"),
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, WORKFLOW)?;
    Ok(WriteResult {
        status: "done",
        detail: with_requirements("wrote .github/workflows/graphify-update.yml"),
    })
}

fn with_requirements(detail: &str) -> String {
    format!("{detail}. {CI_REQUIREMENTS}")
}

pub(crate) fn install_attributes(root: &Path) -> Result<WriteResult, std::io::Error> {
    let path = root.join(".gitattributes");
    let existing = if path.is_file() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let next = crate::managed::rewrite_attributes(&existing);
    if next == existing {
        if has_driver(&existing) {
            register_driver(root);
        }
        return Ok(WriteResult {
            status: "skipped",
            detail: "merge driver already configured".to_string(),
        });
    }
    if next.is_empty() {
        if path.is_file() {
            std::fs::remove_file(&path)?;
        }
    } else {
        std::fs::write(&path, next)?;
    }
    register_driver(root);
    Ok(WriteResult {
        status: "done",
        detail: "set graphify-out/graph.json merge=graphify".to_string(),
    })
}

fn has_driver(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#')
            && !line.is_empty()
            && !line.contains('#')
            && line.contains("graph.json")
            && line.contains("merge=graphify")
    })
}

pub(crate) fn register_driver(root: &Path) {
    // `graphify hook install` may already have registered its own driver.
    // A repository-local value overrides a global or system driver, so keep
    // whichever value git would use. Set Relaybook's only when that key is unset.
    if has_merge_driver(root) {
        return;
    }
    let _ = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "config",
            "--local",
            "merge.graphify.name",
            "graphify graph.json union merge",
        ])
        .status();
    let _ = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "config",
            "--local",
            "merge.graphify.driver",
            "graphify merge-driver %O %A %B",
        ])
        .status();
}

fn has_merge_driver(root: &Path) -> bool {
    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get", "merge.graphify.driver"])
        .output()
    else {
        return false;
    };
    output.status.success()
        && std::str::from_utf8(&output.stdout).is_ok_and(|text| !text.trim().is_empty())
}
