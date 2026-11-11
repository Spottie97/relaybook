use std::fs;
use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;

#[test]
fn setup_ignore_block_matches_in_git_and_stays_put() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    setup(dir.path(), &["setup", "--no-graphify"]);
    let ignore = fs::read_to_string(dir.path().join(".gitignore")).expect("gitignore");
    assert!(ignore.starts_with("# relaybook:start\n"), "{ignore}");
    assert!(ignore.contains("# relaybook:end\n"), "{ignore}");
    assert!(
        !ignore.lines().any(|line| line.contains(" # relaybook")),
        "{ignore}"
    );
    assert!(ignored(dir.path(), "graphify-out/x"));
    assert!(ignored(dir.path(), ".claude/settings.json.graphify-bak"));
    assert!(ignored(dir.path(), ".relaybook/runs/x"));
    assert!(ignored(dir.path(), ".relaybook/ledger.db-wal"));
    assert!(!ignored(dir.path(), ".relaybook/ledger.db"));

    setup(dir.path(), &["setup", "--no-graphify"]);
    assert_eq!(
        fs::read_to_string(dir.path().join(".gitignore")).expect("gitignore"),
        ignore
    );
}

#[test]
fn setup_attribute_block_sets_the_merge_driver() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    setup(dir.path(), &["setup", "--ci", "--no-graphify"]);
    let attributes = fs::read_to_string(dir.path().join(".gitattributes")).expect("attributes");
    assert!(attributes.contains("# relaybook:start\n"), "{attributes}");
    assert!(
        attributes.contains("graphify-out/graph.json merge=graphify\n"),
        "{attributes}"
    );
    assert!(
        !attributes.lines().any(|line| line.contains(" # relaybook")),
        "{attributes}"
    );
    let output = git(
        dir.path(),
        &["check-attr", "merge", "graphify-out/graph.json"],
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr"),
        "",
        "git warned about the attribute"
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout");
    assert!(stdout.contains("merge: graphify"), "{stdout}");
    assert!(!ignored(dir.path(), "graphify-out/x"));
}

#[test]
fn setup_migrates_trailing_hand_fixed_and_unterminated_ignores() {
    let trailing = "keep\ngraphify-out/ # relaybook\n.claude/*.graphify-bak # relaybook\n";
    let hand_fixed = "keep\n# relaybook\ngraphify-out/\n# relaybook\n.relaybook/runs/\n# relaybook\nmy-pattern\n";
    let bare = "keep";
    for original in [trailing, hand_fixed, bare] {
        let dir = tempfile::tempdir().expect("tempdir");
        git_init(dir.path());
        fs::write(dir.path().join(".gitignore"), original).expect("write");
        setup(dir.path(), &["setup", "--no-graphify"]);
        let ignore = fs::read_to_string(dir.path().join(".gitignore")).expect("gitignore");
        assert!(ignore.contains("keep\n"), "{ignore}");
        assert!(ignore.contains("# relaybook:start\n"), "{ignore}");
        assert!(
            !ignore.lines().any(|line| line.contains(" # relaybook")),
            "{ignore}"
        );
        assert!(ignored(dir.path(), "graphify-out/x"), "{ignore}");
        assert!(ignored(dir.path(), ".relaybook/ledger.db-wal"), "{ignore}");
        if original.contains("my-pattern") {
            assert!(ignore.contains("# relaybook\nmy-pattern\n"), "{ignore}");
        }
        let again = ignore.clone();
        setup(dir.path(), &["setup", "--no-graphify"]);
        assert_eq!(
            fs::read_to_string(dir.path().join(".gitignore")).expect("gitignore"),
            again
        );
    }
}

#[test]
fn setup_leaves_an_existing_ignore_where_the_user_wrote_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    git_init(dir.path());
    fs::write(dir.path().join(".gitignore"), "graphify-out/\n").expect("write");
    setup(dir.path(), &["setup", "--no-graphify"]);
    let ignore = fs::read_to_string(dir.path().join(".gitignore")).expect("gitignore");
    assert_eq!(
        ignore
            .lines()
            .filter(|line| line.trim() == "graphify-out/")
            .count(),
        1,
        "{ignore}"
    );
    assert!(ignored(dir.path(), ".claude/settings.json.graphify-bak"));
}

#[test]
fn uninstall_removes_each_ignore_form_and_keeps_user_lines() {
    let forms = [
        "keep\n# relaybook:start\ngraphify-out/\n.claude/*.graphify-bak\n# relaybook:end\n",
        "keep\ngraphify-out/ # relaybook\n",
        "keep\n# relaybook\ngraphify-out/\n# relaybook\nmy-pattern\n",
    ];
    for original in forms {
        let dir = tempfile::tempdir().expect("tempdir");
        git_init(dir.path());
        fs::write(dir.path().join(".gitignore"), original).expect("write");
        relaybook(dir.path())
            .args(["uninstall", "--yes"])
            .assert()
            .success();
        let ignore = fs::read_to_string(dir.path().join(".gitignore")).expect("gitignore");
        assert!(ignore.contains("keep"), "{ignore}");
        assert!(!ignore.contains("graphify-out/"), "{ignore}");
        assert!(!ignore.contains("# relaybook:start"), "{ignore}");
        if original.contains("my-pattern") {
            assert!(ignore.contains("# relaybook\nmy-pattern"), "{ignore}");
        }
    }
}

#[test]
fn uninstall_removes_each_attribute_form() {
    let forms = [
        "keep text=auto\n# relaybook:start\ngraphify-out/graph.json merge=graphify\n# relaybook:end\n",
        "keep text=auto\ngraphify-out/graph.json merge=graphify # relaybook\n",
        "keep text=auto\n# relaybook\ngraphify-out/graph.json merge=graphify\n",
    ];
    for original in forms {
        let dir = tempfile::tempdir().expect("tempdir");
        git_init(dir.path());
        fs::write(dir.path().join(".gitattributes"), original).expect("write");
        relaybook(dir.path())
            .args(["uninstall", "--yes"])
            .assert()
            .success();
        let text = fs::read_to_string(dir.path().join(".gitattributes")).expect("attributes");
        assert!(text.contains("keep text=auto"), "{text}");
        assert!(!text.contains("merge=graphify"), "{text}");
        assert!(!text.contains("# relaybook"), "{text}");
    }
}

#[test]
fn setup_repairs_old_attribute_lines_without_ci() {
    for original in [
        "keep text=auto\ngraphify-out/graph.json merge=graphify # relaybook\n",
        "keep text=auto\n# relaybook\ngraphify-out/graph.json merge=graphify\n",
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        git_init(dir.path());
        fs::write(dir.path().join(".gitattributes"), original).expect("write");
        setup(dir.path(), &["setup", "--no-graphify"]);
        let attributes = fs::read_to_string(dir.path().join(".gitattributes")).expect("attributes");
        assert!(attributes.contains("# relaybook:start\n"), "{attributes}");
        assert!(
            attributes.contains("graphify-out/graph.json merge=graphify\n"),
            "{attributes}"
        );
        assert!(
            !attributes.lines().any(|line| line.contains(" # relaybook")),
            "{attributes}"
        );
        assert!(attributes.contains("keep text=auto"), "{attributes}");
        let output = git(
            dir.path(),
            &["check-attr", "merge", "graphify-out/graph.json"],
        );
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stderr).expect("stderr"), "");
        let stdout = String::from_utf8(output.stdout).expect("stdout");
        assert!(stdout.contains("merge: graphify"), "{stdout}");
        let again = attributes.clone();
        setup(dir.path(), &["setup", "--no-graphify"]);
        assert_eq!(
            fs::read_to_string(dir.path().join(".gitattributes")).expect("attributes"),
            again
        );
    }
}

fn setup(dir: &Path, args: &[&str]) {
    relaybook(dir).args(args).assert().success();
}

fn relaybook(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("relaybook").expect("relaybook binary");
    cmd.current_dir(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    cmd.env("PATH", "/usr/bin:/bin");
    cmd
}

fn git_init(dir: &Path) {
    let output = git(dir, &["init", "-b", "main"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn ignored(dir: &Path, path: &str) -> bool {
    Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(["check-ignore", "-q", "--", path])
        .status()
        .expect("check-ignore")
        .success()
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .output()
        .expect("git")
}
