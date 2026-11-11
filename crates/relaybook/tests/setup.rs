use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use assert_cmd::prelude::*;
use predicates::prelude::PredicateBooleanExt;
use serde_json::Value;

#[test]
fn fresh_setup_is_safe_to_rerun() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = fake_graphify(dir.path(), false);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup"]).assert().success();

    let claude = fs::read_to_string(dir.path().join("CLAUDE.md")).expect("claude");
    assert_eq!(claude.matches("<!-- relaybook:start -->").count(), 1);
    assert!(dir.path().join(".relaybook").join("ledger.db").is_file());
    assert!(dir.path().join("AGENTS.md").is_file());

    setup(dir.path(), &bin, &log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("init: skipped"))
        .stdout(predicates::str::contains("install: skipped"));
    let again = fs::read_to_string(dir.path().join("CLAUDE.md")).expect("claude");
    assert_eq!(again, claude);
}

#[test]
fn missing_graphify_skips_without_failing() {
    let dir = tempfile::tempdir().expect("tempdir");
    relaybook(dir.path())
        .args(["setup"])
        .env("PATH", "/usr/bin:/bin")
        .assert()
        .success()
        .stdout(predicates::str::contains("uv tool install graphifyy"))
        .stdout(predicates::str::contains("install.sh --with-rtk"));
    assert!(dir.path().join("CLAUDE.md").is_file());
    assert!(dir.path().join(".relaybook").join("ledger.db").is_file());
}

#[test]
fn rtk_step_follows_the_fake_on_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = fake_graphify(dir.path(), false);
    copy_fake(&bin, "rtk");
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains("rtk: done"));
}

#[test]
fn no_graphify_does_not_invoke_graphify() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = fake_graphify(dir.path(), false);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--no-graphify"));
    assert!(!log.exists());
}

#[test]
fn failing_graphify_still_finishes_relaybook_steps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = fake_graphify(dir.path(), true);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("failed"));
    assert!(dir.path().join("CLAUDE.md").is_file());
    assert!(dir.path().join(".relaybook").join("ledger.db").is_file());

    let output = setup(dir.path(), &bin, &log, &["setup", "--json"])
        .output()
        .expect("output");
    assert_eq!(output.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(envelope["ok"], false);
    let steps = envelope["data"]["steps"].as_array().expect("steps");
    assert!(steps.iter().any(|step| {
        step["name"] == "init" && (step["status"] == "done" || step["status"] == "skipped")
    }));
    assert!(steps.iter().any(|step| step["status"] == "failed"));
}

#[test]
fn merge_and_rebase_refresh_the_graph_and_keep_existing_hooks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git_init(root);
    let bin = fake_graphify(root, false);
    let log = root.join("graphify.log");
    let marker = root.join("hook-marker.log");
    write_hook(
        &root.join(".git").join("hooks").join("post-merge"),
        &format!("#!/bin/sh\necho merge >> '{}'\n", marker.display()),
    );
    write_hook(
        &root.join(".git").join("hooks").join("post-rewrite"),
        &format!("#!/bin/sh\necho rewrite >> '{}'\n", marker.display()),
    );

    setup(root, &bin, &log, &["setup"]).assert().success();
    setup(root, &bin, &log, &["setup"]).assert().success();
    let merged =
        fs::read_to_string(root.join(".git").join("hooks").join("post-merge")).expect("hook");
    assert_eq!(merged.matches("# relaybook:hooks:start").count(), 1);
    assert!(merged.contains("echo merge >>"));

    let before = count_log(&log, "update .");
    commit(root, "base.txt", "base");
    git(root, &["checkout", "-b", "feature"]);
    commit(root, "feature.txt", "feature");
    git(root, &["checkout", "main"]);
    commit(root, "main.txt", "main");
    let merge = git_env(root, &bin, &log, &["merge", "--no-edit", "feature"]);
    assert!(
        merge.status.success(),
        "{}",
        String::from_utf8_lossy(&merge.stderr)
    );
    assert!(
        wait_for(&log, "update .", before + 1),
        "graphify log: {}",
        read_lossy(&log)
    );
    assert!(
        read_lossy(&marker).contains("merge"),
        "marker: {}",
        read_lossy(&marker)
    );

    let origin = tempfile::tempdir().expect("origin");
    git_init(origin.path());
    commit(origin.path(), "shared.txt", "origin");
    let work = tempfile::tempdir().expect("work");
    git(
        root,
        &[
            "clone",
            origin.path().to_str().expect("path"),
            work.path().to_str().expect("path"),
        ],
    );
    configure(work.path());
    let work_bin = fake_graphify(work.path(), false);
    let work_log = work.path().join("graphify.log");
    let work_marker = work.path().join("hook-marker.log");
    write_hook(
        &work.path().join(".git").join("hooks").join("post-rewrite"),
        &format!("#!/bin/sh\necho rewrite >> '{}'\n", work_marker.display()),
    );
    setup(work.path(), &work_bin, &work_log, &["setup"])
        .assert()
        .success();
    commit(origin.path(), "remote.txt", "remote");
    commit(work.path(), "local.txt", "local");
    let before_rebase = count_log(&work_log, "update .");
    let pull = git_env(
        work.path(),
        &work_bin,
        &work_log,
        &["pull", "--rebase", "origin", "main"],
    );
    assert!(
        pull.status.success(),
        "{}",
        String::from_utf8_lossy(&pull.stderr)
    );
    assert!(
        wait_for(&work_log, "update .", before_rebase + 1),
        "rebase log: {}",
        read_lossy(&work_log)
    );
    assert!(
        read_lossy(&work_marker).contains("rewrite"),
        "marker: {}",
        read_lossy(&work_marker)
    );
}

#[test]
fn custom_hook_managers_are_left_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    git(dir.path(), &["config", "core.hooksPath", ".githooks"]);
    let bin = fake_graphify(dir.path(), false);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("core.hooksPath"));
    assert!(
        !dir.path()
            .join(".git")
            .join("hooks")
            .join("post-merge")
            .exists()
    );
    assert!(!dir.path().join(".githooks").join("post-merge").exists());

    let husky = tempfile::tempdir().expect("husky");
    git_init(husky.path());
    fs::create_dir(husky.path().join(".husky")).expect("husky");
    let husky_bin = fake_graphify(husky.path(), false);
    let husky_log = husky.path().join("graphify.log");
    setup(husky.path(), &husky_bin, &husky_log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Husky"));
    assert!(
        !husky
            .path()
            .join(".git")
            .join("hooks")
            .join("post-merge")
            .exists()
    );

    let left = tempfile::tempdir().expect("lefthook");
    git_init(left.path());
    fs::write(left.path().join("lefthook.yml"), "pre-commit:\n").expect("lefthook");
    let left_bin = fake_graphify(left.path(), false);
    let left_log = left.path().join("graphify.log");
    setup(left.path(), &left_bin, &left_log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Lefthook"));
    assert!(
        !left
            .path()
            .join(".git")
            .join("hooks")
            .join("post-merge")
            .exists()
    );
}

#[test]
fn no_hooks_skips_hook_installation() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let bin = fake_graphify(dir.path(), false);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup", "--no-hooks"])
        .assert()
        .success()
        .stdout(predicates::str::contains("--no-hooks"));
    assert!(
        !dir.path()
            .join(".git")
            .join("hooks")
            .join("post-merge")
            .exists()
    );
    let text = fs::read_to_string(&log).expect("log");
    assert!(!text.contains("hook install"));
    assert!(text.contains("update ."));
}

#[test]
fn ci_workflow_is_written_once_and_never_overwrites() {
    let dir = tempfile::tempdir().expect("tempdir");
    setup_plain(dir.path(), &["setup", "--ci", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains("GRAPHIFY_PR_TOKEN"))
        .stdout(predicates::str::contains(
            "Allow GitHub Actions to create and approve pull requests",
        ));
    let path = dir
        .path()
        .join(".github")
        .join("workflows")
        .join("graphify-update.yml");
    let text = fs::read_to_string(&path).expect("workflow");
    assert!(text.contains("paths-ignore:"));
    assert!(text.contains("graphify-out/**"));
    assert!(text.contains("graphify extract ."));
    assert!(text.contains("graphify update ."));
    assert!(text.contains("peter-evans/create-pull-request"));
    assert!(text.contains("secrets.GRAPHIFY_PR_TOKEN || github.token"));
    let attributes = fs::read_to_string(dir.path().join(".gitattributes")).expect("attributes");
    assert!(attributes.contains("graphify-out/graph.json merge=graphify"));

    setup_plain(dir.path(), &["setup", "--ci", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ci workflow: skipped"));
    assert_eq!(fs::read_to_string(&path).expect("workflow"), text);

    let existing = tempfile::tempdir().expect("existing");
    let existing_path = existing
        .path()
        .join(".github")
        .join("workflows")
        .join("graphify-update.yml");
    fs::create_dir_all(existing_path.parent().expect("parent")).expect("mkdir");
    fs::write(&existing_path, "custom: true\n").expect("write");
    setup_plain(existing.path(), &["setup", "--ci", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ci workflow: skipped"));
    assert_eq!(
        fs::read_to_string(&existing_path).expect("read"),
        "custom: true\n"
    );
}

#[test]
fn legacy_hook_markers_are_replaced_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    let hook = dir.path().join(".git").join("hooks").join("post-merge");
    write_hook(
        &hook,
        "#!/bin/sh\n# agentmesh:hooks:start\necho old\n# agentmesh:hooks:end\necho kept\n",
    );
    let bin = fake_graphify(dir.path(), false);
    let log = dir.path().join("graphify.log");
    setup(dir.path(), &bin, &log, &["setup"]).assert().success();
    let text = fs::read_to_string(&hook).expect("hook");
    assert_eq!(text.matches("# relaybook:hooks:start").count(), 1);
    assert!(!text.contains("agentmesh"));
    assert!(text.contains("echo kept"));
    let again = text.clone();
    setup(dir.path(), &bin, &log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("post-merge: already installed"));
    assert_eq!(fs::read_to_string(&hook).expect("hook"), again);
}

#[test]
fn shell_hook_with_exit_still_refreshes_the_graph() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git_init(root);
    let bin = fake_graphify(root, false);
    let log = root.join("graphify.log");
    let marker = root.join("hook-marker.log");
    write_hook(
        &root.join(".git").join("hooks").join("post-merge"),
        &format!("#!/bin/sh\necho merge >> '{}'\nexit 0\n", marker.display()),
    );
    setup(root, &bin, &log, &["setup"]).assert().success();
    let hook =
        fs::read_to_string(root.join(".git").join("hooks").join("post-merge")).expect("hook");
    let block = hook.find("# relaybook:hooks:start").expect("block");
    let exit_at = hook.find("exit 0").expect("exit");
    assert!(block < exit_at, "{hook}");
    commit(root, "base.txt", "base");
    git(root, &["checkout", "-b", "feature"]);
    commit(root, "feature.txt", "feature");
    git(root, &["checkout", "main"]);
    commit(root, "main.txt", "main");
    let before = count_log(&log, "update .");
    let merge = git_env(root, &bin, &log, &["merge", "--no-edit", "feature"]);
    assert!(
        merge.status.success(),
        "{}",
        String::from_utf8_lossy(&merge.stderr)
    );
    assert!(
        wait_for(&log, "update .", before + 1),
        "{}",
        read_lossy(&log)
    );
    assert!(read_lossy(&marker).contains("merge"));

    let again =
        fs::read_to_string(root.join(".git").join("hooks").join("post-merge")).expect("hook");
    setup(root, &bin, &log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains("post-merge: already installed"));
    assert_eq!(
        fs::read_to_string(root.join(".git").join("hooks").join("post-merge")).expect("hook"),
        again
    );
}

#[test]
fn python_hook_is_left_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git_init(root);
    let body = "#!/usr/bin/env python3\nprint('hook')\n";
    let merge = root.join(".git").join("hooks").join("post-merge");
    let rewrite = root.join(".git").join("hooks").join("post-rewrite");
    write_hook(&merge, body);
    write_hook(&rewrite, body);
    let bin = fake_graphify(root, false);
    let log = root.join("graphify.log");
    setup(root, &bin, &log, &["setup"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "post-merge: skipped (python3 hook; add 'graphify update . &' yourself)",
        ))
        .stdout(predicates::str::contains(
            "post-rewrite: skipped (python3 hook; add 'graphify update . &' yourself)",
        ));
    assert_eq!(fs::read_to_string(&merge).expect("merge"), body);
    assert_eq!(fs::read_to_string(&rewrite).expect("rewrite"), body);
}

#[test]
fn setup_merges_the_prompt_hook_without_duplicating_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join(".claude").join("settings.json");
    fs::create_dir_all(settings.parent().expect("parent")).expect("mkdir");
    fs::write(
        &settings,
        r#"{"keep":true,"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"graphify"}]}]}}"#,
    )
    .expect("write");

    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "prompt hook: done (relaybook is not on PATH; the prompt hook stays silent until it is)",
        ));

    let value: Value =
        serde_json::from_str(&fs::read_to_string(&settings).expect("read")).expect("json");
    assert_eq!(value["keep"], true);
    assert_eq!(value["hooks"]["PreToolUse"][0]["matcher"], "Bash");
    assert_eq!(
        value["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "graphify"
    );
    let prompt = value["hooks"]["UserPromptSubmit"]
        .as_array()
        .expect("prompt");
    assert_eq!(prompt.len(), 1);
    assert_eq!(prompt[0]["hooks"][0]["command"], "relaybook hook prompt");

    let first = fs::read(&settings).expect("read");
    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "prompt hook: done (relaybook is not on PATH; the prompt hook stays silent until it is)",
        ));
    assert_eq!(fs::read(&settings).expect("read"), first);
}

#[test]
fn setup_installs_and_preserves_the_claude_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir
        .path()
        .join(".claude")
        .join("commands")
        .join("relaybook.md");
    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "claude command: done (wrote .claude/commands/relaybook.md)",
        ));
    let body = fs::read_to_string(&path).expect("command");
    let front = claude_front_matter(&body);
    assert_eq!(
        front.get("description").map(String::as_str),
        Some("Relaybook: mode, checkpoints, auto runs, inbox")
    );
    assert_eq!(
        front.get("allowed-tools").map(String::as_str),
        Some("Bash(relaybook:*)")
    );
    assert!(body.contains("AskUserQuestion"), "{body}");
    assert!(body.contains("run_in_background"), "{body}");

    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "claude command: skipped (already installed)",
        ));
    assert_eq!(fs::read_to_string(&path).expect("command"), body);

    fs::write(&path, body.replace("Relaybook:", "Old:")).expect("edit");
    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "claude command: done (wrote .claude/commands/relaybook.md)",
        ));
    assert_eq!(fs::read_to_string(&path).expect("command"), body);

    fs::write(&path, "custom command\n").expect("custom");
    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "claude command: skipped (file has no relaybook:managed marker)",
        ));
    assert_eq!(
        fs::read_to_string(&path).expect("command"),
        "custom command\n"
    );
}

#[test]
fn setup_upgrades_an_old_claude_command_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir
        .path()
        .join(".claude")
        .join("commands")
        .join("relaybook.md");
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(
        &path,
        "<!-- relaybook:managed -->\n---\ndescription: Relaybook: mode, checkpoints, auto runs, inbox\n---\n\nold\n",
    )
    .expect("old");
    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "claude command: done (wrote .claude/commands/relaybook.md)",
        ));
    let body = fs::read_to_string(&path).expect("command");
    assert!(body.starts_with("---\n"), "{body}");
    assert!(
        body.split("\n---\n")
            .next()
            .expect("front matter")
            .contains("# relaybook:managed"),
        "{body}"
    );
    assert_eq!(
        claude_front_matter(&body)
            .get("description")
            .map(String::as_str),
        Some("Relaybook: mode, checkpoints, auto runs, inbox")
    );
}

fn claude_front_matter(body: &str) -> std::collections::BTreeMap<String, String> {
    assert!(
        body.starts_with("---\n"),
        "front matter must start on line 1: {body}"
    );
    let rest = &body["---\n".len()..];
    let end = rest.find("\n---\n").expect("closing front matter");
    let mut fields = std::collections::BTreeMap::new();
    for line in rest[..end].lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw) = line.split_once(':').expect("front matter field");
        let raw = raw.trim();
        let value = if let Some(quoted) = raw.strip_prefix('"') {
            quoted.strip_suffix('"').expect("closing quote").to_string()
        } else {
            assert!(
                !raw.contains(": "),
                "unquoted value contains a colon: {line}"
            );
            raw.to_string()
        };
        fields.insert(key.trim().to_string(), value);
    }
    fields
}

#[test]
fn no_commands_skips_the_claude_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    setup_plain(dir.path(), &["setup", "--no-graphify", "--no-commands"])
        .assert()
        .success()
        .stdout(
            predicates::str::contains("claude command: skipped (--no-commands)").and(
                predicates::str::contains("cursor command: skipped (--no-commands)"),
            ),
        );
    assert!(
        !dir.path()
            .join(".claude")
            .join("commands")
            .join("relaybook.md")
            .exists()
    );
    assert!(
        !dir.path()
            .join(".cursor")
            .join("commands")
            .join("relaybook.md")
            .exists()
    );
}

#[test]
fn setup_installs_and_preserves_the_cursor_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir
        .path()
        .join(".cursor")
        .join("commands")
        .join("relaybook.md");
    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "cursor command: done (wrote .cursor/commands/relaybook.md)",
        ));
    let body = fs::read_to_string(&path).expect("command");
    assert!(body.contains("relaybook:managed"), "{body}");
    assert!(body.contains("relaybook status"), "{body}");

    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "cursor command: skipped (already installed)",
        ));
    assert_eq!(fs::read_to_string(&path).expect("command"), body);

    fs::write(&path, body.replace("/relaybook", "/old")).expect("edit");
    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "cursor command: done (wrote .cursor/commands/relaybook.md)",
        ));
    assert_eq!(fs::read_to_string(&path).expect("command"), body);

    fs::write(&path, "custom cursor command\n").expect("custom");
    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "cursor command: skipped (file has no relaybook:managed marker)",
        ));
    assert_eq!(
        fs::read_to_string(&path).expect("command"),
        "custom cursor command\n"
    );
}

#[test]
fn prompt_hook_reports_installed_when_relaybook_is_on_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    copy_fake(&bin, "relaybook");
    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .env("PATH", prepend_path(&bin))
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "prompt hook: done (added UserPromptSubmit)",
        ));
    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .env("PATH", prepend_path(&bin))
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "prompt hook: skipped (UserPromptSubmit already installed)",
        ));
}

#[test]
fn setup_guards_graphify_hooks_and_ignores_local_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join(".claude").join("settings.json");
    fs::create_dir_all(settings.parent().expect("parent")).expect("mkdir");
    fs::write(
        &settings,
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash|Grep","hooks":[{"type":"command","command":"/tmp/graphify hook-guard search","timeout":10}]},{"matcher":"Read|Glob","hooks":[{"type":"command","command":"graphify hook-guard read --strict"}]}]}}"#,
    )
    .expect("write");

    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "graphify guards: done (wrapped PreToolUse)",
        ))
        .stdout(predicates::str::contains(
            "gitignore: done (ignored .claude/*.graphify-bak and graphify-out/)",
        ));

    let value: Value =
        serde_json::from_str(&fs::read_to_string(&settings).expect("read")).expect("json");
    assert_eq!(
        value["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "command -v graphify >/dev/null 2>&1 && graphify hook-guard search || true"
    );
    assert_eq!(value["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"], 10);
    assert_eq!(
        value["hooks"]["PreToolUse"][1]["hooks"][0]["command"],
        "command -v graphify >/dev/null 2>&1 && graphify hook-guard read --strict || true"
    );
    let ignore = fs::read_to_string(dir.path().join(".gitignore")).expect("gitignore");
    assert!(
        ignore.contains(".claude/*.graphify-bak # relaybook\n"),
        "{ignore}"
    );
    assert!(ignore.contains("graphify-out/ # relaybook\n"), "{ignore}");

    let wrapped = value["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .expect("command")
        .to_string();
    let printed = posix_shell()
        .arg("-c")
        .arg(&wrapped)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("wrapper");
    assert!(printed.status.success());
    assert_eq!(printed.stdout, b"");

    let fake = dir.path().join("bin");
    fs::create_dir_all(&fake).expect("mkdir");
    fs::write(
        fake.join("graphify"),
        "#!/bin/sh\nprintf '%s\\n' '{\"ok\":true}'\n",
    )
    .expect("write");
    make_executable(&fake.join("graphify"));
    let printed = posix_shell()
        .arg("-c")
        .arg(&wrapped)
        .env("PATH", format!("{}:/usr/bin:/bin", fake.display()))
        .output()
        .expect("wrapper");
    assert!(printed.status.success());
    assert_eq!(printed.stdout, b"{\"ok\":true}\n");

    let first = fs::read(&settings).expect("read");
    setup_plain(dir.path(), &["setup", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "graphify guards: skipped (PreToolUse already guarded)",
        ))
        .stdout(predicates::str::contains(
            "gitignore: skipped (already ignored)",
        ));
    assert_eq!(fs::read(&settings).expect("read"), first);
}

#[test]
fn fake_graphify_install_gets_guarded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).expect("mkdir");
    copy_fake(&bin, "graphify");

    setup_plain(dir.path(), &["setup", "--no-hooks"])
        .env("PATH", prepend_path(&bin))
        .env("RELAYBOOK_FAKE_GRAPHIFY", "claude-settings")
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "graphify guards: done (wrapped PreToolUse)",
        ));
    let value: Value = serde_json::from_str(
        &fs::read_to_string(dir.path().join(".claude").join("settings.json")).expect("read"),
    )
    .expect("json");
    assert_eq!(
        value["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "command -v graphify >/dev/null 2>&1 && graphify hook-guard search || true"
    );
    assert_eq!(value["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"], 10);
}

#[test]
fn ci_setup_leaves_graphify_output_tracked() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join(".gitignore"), "graphify-out/\nkeep\n").expect("write");
    setup_plain(dir.path(), &["setup", "--ci", "--no-graphify"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "gitignore: done (graphify-out stays tracked)",
        ));
    let ignore = fs::read_to_string(dir.path().join(".gitignore")).expect("gitignore");
    assert!(
        !ignore.lines().any(|line| line.trim() == "graphify-out/"),
        "{ignore}"
    );
    assert!(ignore.contains(".claude/*.graphify-bak"), "{ignore}");
    assert!(ignore.contains("keep"), "{ignore}");
}

#[test]
fn no_claude_hook_skips_the_prompt_hook() {
    let dir = tempfile::tempdir().expect("tempdir");
    setup_plain(dir.path(), &["setup", "--no-graphify", "--no-claude-hook"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "prompt hook: skipped (--no-claude-hook)",
        ));
    assert!(!dir.path().join(".claude").join("settings.json").exists());
}

fn setup(dir: &Path, bin: &Path, log: &Path, args: &[&str]) -> Command {
    let mut cmd = relaybook(dir);
    cmd.args(args);
    with_graphify(&mut cmd, bin, log);
    cmd
}

fn setup_plain(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = relaybook(dir);
    cmd.args(args);
    cmd.env("PATH", "/usr/bin:/bin");
    cmd
}

fn relaybook(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("relaybook").expect("relaybook binary");
    cmd.current_dir(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    cmd
}

fn fake_graphify(dir: &Path, fail: bool) -> PathBuf {
    let bin = dir.join("fake-bin");
    copy_fake(&bin, "graphify");
    if cfg!(windows) {
        fs::write(
            bin.join("graphify"),
            "#!/bin/sh\necho \"$*\" >> \"$GRAPHIFY_LOG\"\necho boom >&2\nif [ -f \"$(dirname \"$0\")/graphify.fail\" ]; then exit 1; fi\nexit 0\n",
        )
        .expect("hook script");
    }
    if fail {
        fs::write(bin.join("graphify.fail"), "1").expect("fail marker");
    }
    bin
}

fn copy_fake(dir: &Path, name: &str) {
    fs::create_dir_all(dir).expect("mkdir");
    let src = PathBuf::from(env!("CARGO_BIN_EXE_fake"));
    let dest = if cfg!(windows) {
        dir.join(format!("{name}.exe"))
    } else {
        dir.join(name)
    };
    fs::copy(src, &dest).expect("copy fake");
    make_executable(&dest);
}

fn prepend_path(bin: &Path) -> std::ffi::OsString {
    let mut path = std::ffi::OsString::from(bin);
    if let Some(rest) = std::env::var_os("PATH") {
        path.push(if cfg!(windows) { ";" } else { ":" });
        path.push(rest);
    }
    path
}

fn with_graphify(cmd: &mut Command, bin: &Path, log: &Path) {
    cmd.env("PATH", prepend_path(bin));
    cmd.env("GRAPHIFY_LOG", log);
}

fn posix_shell() -> Command {
    if cfg!(windows) {
        let git_bash = Path::new(r"C:\Program Files\Git\bin\sh.exe");
        if git_bash.is_file() {
            return Command::new(git_bash);
        }
    }
    Command::new("sh")
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).expect("meta").permissions();
        permissions.set_mode(0o755);
        let _ = fs::set_permissions(path, permissions);
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn git_init(dir: &Path) {
    git(dir, &["init", "-b", "main"]);
    configure(dir);
}

fn configure(dir: &Path) {
    git(dir, &["config", "user.name", "Relaybook"]);
    git(dir, &["config", "user.email", "relaybook@example.com"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn commit(dir: &Path, file: &str, message: &str) {
    fs::write(dir.join(file), message).expect("write");
    git(dir, &["add", file]);
    git(dir, &["commit", "-m", message]);
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "{args:?} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git_env(dir: &Path, bin: &Path, log: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    with_graphify(&mut cmd, bin, log);
    cmd.output().expect("git")
}

fn write_hook(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, body).expect("write");
    make_executable(path);
}

fn count_log(path: &Path, needle: &str) -> usize {
    if !path.is_file() {
        return 0;
    }
    fs::read_to_string(path)
        .expect("read")
        .lines()
        .filter(|line| line.contains(needle))
        .count()
}

fn wait_for(path: &Path, needle: &str, at_least: usize) -> bool {
    for _ in 0..40 {
        if count_log(path, needle) >= at_least {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn read_lossy(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}
