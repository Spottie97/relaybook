use std::fs;
use std::path::{Path, PathBuf};
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
fn setup_keeps_an_existing_merge_driver() {
    let custom_driver = "\"/usr/bin/python3\" -m graphify merge-driver %O %A %B";
    let custom_name = "graphify union";

    let repaired = tempfile::tempdir().expect("tempdir");
    git_init(repaired.path());
    git_ok(
        repaired.path(),
        &["config", "merge.graphify.driver", custom_driver],
    );
    git_ok(
        repaired.path(),
        &["config", "merge.graphify.name", custom_name],
    );
    fs::write(
        repaired.path().join(".gitattributes"),
        "graphify-out/graph.json merge=graphify # relaybook\n",
    )
    .expect("write");
    setup(repaired.path(), &["setup", "--no-graphify"]);
    assert_eq!(
        config_get(repaired.path(), "merge.graphify.driver"),
        custom_driver
    );
    assert_eq!(
        config_get(repaired.path(), "merge.graphify.name"),
        custom_name
    );
    let attributes =
        fs::read_to_string(repaired.path().join(".gitattributes")).expect("attributes");
    assert!(
        attributes.contains("graphify-out/graph.json merge=graphify\n"),
        "{attributes}"
    );

    let ci = tempfile::tempdir().expect("tempdir");
    git_init(ci.path());
    git_ok(
        ci.path(),
        &["config", "merge.graphify.driver", custom_driver],
    );
    git_ok(ci.path(), &["config", "merge.graphify.name", custom_name]);
    setup(ci.path(), &["setup", "--ci", "--no-graphify"]);
    assert_eq!(
        config_get(ci.path(), "merge.graphify.driver"),
        custom_driver
    );
    assert_eq!(config_get(ci.path(), "merge.graphify.name"), custom_name);
    let attributes = fs::read_to_string(ci.path().join(".gitattributes")).expect("attributes");
    assert!(
        attributes.contains("graphify-out/graph.json merge=graphify\n"),
        "{attributes}"
    );

    let fresh = tempfile::tempdir().expect("tempdir");
    git_init(fresh.path());
    setup(fresh.path(), &["setup", "--ci", "--no-graphify"]);
    assert_eq!(
        config_get(fresh.path(), "merge.graphify.driver"),
        "graphify merge-driver %O %A %B"
    );
    assert_eq!(
        config_get(fresh.path(), "merge.graphify.name"),
        "graphify graph.json union merge"
    );
}

#[test]
fn setup_keeps_a_global_merge_driver() {
    // Child processes only. GIT_CONFIG_COUNT and its pushInsteadOf pairs stay inherited.
    let root = tempfile::tempdir().expect("tempdir");
    let repo = root.path().join("repo");
    let global = root.path().join("global.gitconfig");
    fs::create_dir(&repo).expect("repo");
    fs::write(&global, "").expect("global gitconfig");
    fs::create_dir(root.path().join("home")).expect("home");
    let home_before = real_global_configs();
    let system_before = fs::read("/etc/gitconfig").ok();

    let custom_driver = "\"/usr/bin/python3\" -m graphify merge-driver %O %A %B";
    let custom_name = "graphify union";
    let init = git_global(&repo, &global, &["init", "-b", "main"]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    write_global_config(&global, "merge.graphify.driver", custom_driver);
    write_global_config(&global, "merge.graphify.name", custom_name);

    setup_with_global(&repo, &global, &["setup", "--ci", "--no-graphify"]);

    let local = fs::read_to_string(repo.join(".git/config")).expect("local gitconfig");
    assert!(
        !local.contains("[merge \"graphify\"]"),
        "local config overrode the global driver:\n{local}"
    );
    assert_local_absent(&repo, &global, "merge.graphify.driver");
    assert_local_absent(&repo, &global, "merge.graphify.name");
    assert_eq!(
        effective_config(&repo, &global, "merge.graphify.driver"),
        custom_driver
    );
    assert_eq!(
        effective_config(&repo, &global, "merge.graphify.name"),
        custom_name
    );
    assert_eq!(file_config(&global, "merge.graphify.driver"), custom_driver);
    assert_eq!(file_config(&global, "merge.graphify.name"), custom_name);
    let attributes = fs::read_to_string(repo.join(".gitattributes")).expect("attributes");
    assert!(
        attributes.contains("graphify-out/graph.json merge=graphify\n"),
        "{attributes}"
    );
    assert!(
        real_global_configs() == home_before,
        "real ~/.gitconfig was modified"
    );
    assert!(
        fs::read("/etc/gitconfig").ok() == system_before,
        "system gitconfig was modified"
    );
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

fn child_path() -> std::ffi::OsString {
    if cfg!(windows) {
        let dir = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .find(|dir| dir.join("git.exe").is_file())
            .expect("git.exe on PATH");
        std::env::join_paths([dir]).expect("git.exe on PATH")
    } else {
        "/usr/bin:/bin".into()
    }
}

fn relaybook(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("relaybook").expect("relaybook binary");
    cmd.current_dir(dir);
    cmd.env_remove("GIT_DIR");
    cmd.env_remove("GIT_WORK_TREE");
    cmd.env("PATH", child_path());
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

#[track_caller]
fn git_ok(dir: &Path, args: &[&str]) {
    let output = git(dir, args);
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[track_caller]
fn config_get(dir: &Path, key: &str) -> String {
    let output = git(dir, &["config", "--local", "--get", key]);
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

fn real_global_configs() -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let mut dirs = Vec::new();
    for key in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(key) {
            let dir = PathBuf::from(value);
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    if let (Some(drive), Some(path)) = (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
    {
        let mut joined = drive;
        joined.push(path);
        let dir = PathBuf::from(joined);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    assert!(
        !dirs.is_empty(),
        "no HOME, USERPROFILE or HOMEDRIVE+HOMEPATH"
    );

    let mut found = Vec::new();
    for dir in dirs {
        let path = dir.join(".gitconfig");
        let bytes = fs::read(&path).ok();
        found.push((path, bytes));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(xdg).join("git").join("config");
        let bytes = fs::read(&path).ok();
        found.push((path, bytes));
    } else if let Some(home) = std::env::var_os("HOME") {
        let path = PathBuf::from(home)
            .join(".config")
            .join("git")
            .join("config");
        let bytes = fs::read(&path).ok();
        found.push((path, bytes));
    }
    found
}

fn isolate(cmd: &mut Command, home: &Path) {
    cmd.env("HOME", home);
    cmd.env("USERPROFILE", home);
    cmd.env("XDG_CONFIG_HOME", home);
}

fn git_global(dir: &Path, global: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args);
    isolate(&mut cmd, &global.parent().unwrap().join("home"));
    cmd.output().expect("git")
}

fn write_global_config(global: &Path, key: &str, value: &str) {
    let mut cmd = Command::new("git");
    cmd.env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["config", "--file"])
        .arg(global)
        .arg(key)
        .arg(value);
    isolate(&mut cmd, &global.parent().unwrap().join("home"));
    let output = cmd.output().expect("git config --file");
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[track_caller]
fn file_config(global: &Path, key: &str) -> String {
    let mut cmd = Command::new("git");
    cmd.env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["config", "--file"])
        .arg(global)
        .args(["--get", key]);
    isolate(&mut cmd, &global.parent().unwrap().join("home"));
    let output = cmd.output().expect("git config --file --get");
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

#[track_caller]
fn effective_config(dir: &Path, global: &Path, key: &str) -> String {
    let output = git_global(dir, global, &["config", "--get", key]);
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

#[track_caller]
fn assert_local_absent(dir: &Path, global: &Path, key: &str) {
    let output = git_global(dir, global, &["config", "--local", "--get", key]);
    assert!(
        !output.status.success(),
        "local {key} was set: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn setup_with_global(dir: &Path, global: &Path, args: &[&str]) {
    let mut cmd = Command::cargo_bin("relaybook").expect("relaybook binary");
    cmd.current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("PATH", child_path())
        .args(args);
    isolate(&mut cmd, &global.parent().unwrap().join("home"));
    cmd.assert().success();
}
