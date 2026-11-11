use std::path::Path;
use std::process::Command;

const WORKFLOW: &str = include_str!("../templates/graphify-update.yml");
pub(crate) const TEMPLATE: &str = WORKFLOW;
const ATTRIBUTE: &str = "graphify-out/graph.json merge=graphify";
pub(crate) const ATTRIBUTE_LINE: &str = ATTRIBUTE;
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
    if has_driver(&existing) {
        register_driver(root);
        return Ok(WriteResult {
            status: "skipped",
            detail: "merge driver already configured".to_string(),
        });
    }
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(ATTRIBUTE);
    next.push_str(" # relaybook\n");
    std::fs::write(&path, next)?;
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
            && line.contains("graph.json")
            && line.contains("merge=graphify")
    })
}

fn register_driver(root: &Path) {
    let _ = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "config",
            "merge.graphify.name",
            "graphify graph.json union merge",
        ])
        .status();
    let _ = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "config",
            "merge.graphify.driver",
            "graphify merge-driver %O %A %B",
        ])
        .status();
}
