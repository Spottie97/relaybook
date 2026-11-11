#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;

fn script(home: &Path, bin: &Path) -> Command {
    let mut cmd = Command::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh"));
    cmd.env("HOME", home);
    cmd.env("XDG_DATA_HOME", home.join("share"));
    cmd.env("RELAYBOOK_INSTALL_DIR", bin);
    cmd.env("PATH", format!("/usr/bin:/bin:{}", bin.display()));
    cmd
}

fn fake_curl(dir: &Path) {
    fs::create_dir_all(dir).expect("mkdir");
    let path = dir.join("curl");
    fs::write(
        &path,
        "#!/bin/sh\nprev=\nout=\nfor arg in \"$@\"; do\n  if [ \"$prev\" = \"-o\" ]; then out=$arg; fi\n  prev=$arg\ndone\ndir=$(mktemp -d)\nprintf '%s\\n' '#!/bin/sh' 'echo relaybook 0.0.0-test' > \"$dir/relaybook\"\nchmod +x \"$dir/relaybook\"\ntar -czf \"$out\" -C \"$dir\" relaybook\nrm -rf \"$dir\"\n",
    )
    .expect("write");
    chmod(&path);
}

fn chmod(path: &Path) {
    let mut permissions = fs::metadata(path).expect("meta").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod");
}

#[test]
fn install_then_uninstall_removes_only_the_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let bin = dir.path().join("bin");
    let fakes = dir.path().join("fakes");
    fs::create_dir_all(&home).expect("home");
    fake_curl(&fakes);
    script(&home, &bin)
        .env("PATH", format!("{}:/usr/bin:/bin", fakes.display()))
        .args(["--no-graphify", "--yes"])
        .assert()
        .success();
    assert!(bin.join("relaybook").is_file());
    let manifest = fs::read_to_string(home.join("share/relaybook/install.json")).expect("manifest");
    assert!(manifest.contains("\"graphify\": \"none\""), "{manifest}");

    script(&home, &bin)
        .args(["--uninstall", "--yes"])
        .assert()
        .success()
        .stdout(predicates::str::contains("removed"));
    assert!(!bin.join("relaybook").exists());
    assert!(!home.join("share/relaybook/install.json").exists());

    script(&home, &bin)
        .args(["--uninstall"])
        .assert()
        .success()
        .stdout(predicates::str::contains("nothing installed"));
}

#[test]
fn pre_existing_graphify_is_kept_and_cargo_copy_is_named() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let bin = dir.path().join("bin");
    let fakes = dir.path().join("fakes");
    fs::create_dir_all(&home).expect("home");
    fake_curl(&fakes);
    let graphify = fakes.join("graphify");
    fs::write(&graphify, "#!/bin/sh\nexit 0\n").expect("write");
    chmod(&graphify);
    let uv = fakes.join("uv");
    fs::write(&uv, "#!/bin/sh\necho uv >> \"$UV_LOG\"\nexit 0\n").expect("write");
    chmod(&uv);
    let log = dir.path().join("uv.log");
    script(&home, &bin)
        .env("PATH", format!("{}:/usr/bin:/bin", fakes.display()))
        .env("UV_LOG", &log)
        .args(["--no-graphify", "--yes"])
        .assert()
        .success();
    let manifest = fs::read_to_string(home.join("share/relaybook/install.json")).expect("manifest");
    assert!(manifest.contains("pre-existing"), "{manifest}");

    script(&home, &bin)
        .env("PATH", format!("{}:/usr/bin:/bin", fakes.display()))
        .env("UV_LOG", &log)
        .args(["--uninstall", "--yes"])
        .assert()
        .success()
        .stdout(predicates::str::contains("kept graphify"));
    assert!(!log.exists() || fs::read_to_string(&log).unwrap_or_default().is_empty());

    let cargo = dir.path().join("home/.cargo/bin");
    fs::create_dir_all(&cargo).expect("mkdir");
    fs::write(cargo.join("relaybook"), "#!/bin/sh\necho cargo-copy\n").expect("write");
    chmod(&cargo.join("relaybook"));
    fs::create_dir_all(home.join("share/relaybook")).expect("mkdir");
    fs::write(
        home.join("share/relaybook/install.json"),
        format!(
            "{{\n  \"binary\": \"{}\",\n  \"graphify\": \"none\",\n  \"rtk\": \"none\",\n  \"rtk_init\": false\n}}\n",
            bin.join("relaybook").display()
        ),
    )
    .expect("manifest");
    fs::write(bin.join("relaybook"), "#!/bin/sh\necho installed\n").expect("binary");
    chmod(&bin.join("relaybook"));
    script(&home, &bin)
        .env(
            "PATH",
            format!("{}:{}:/usr/bin:/bin", cargo.display(), bin.display()),
        )
        .args(["--uninstall", "--yes"])
        .assert()
        .success()
        .stdout(predicates::str::contains("cargo uninstall relaybook"));
    assert!(cargo.join("relaybook").is_file());
    assert!(!bin.join("relaybook").exists());
}

#[test]
fn no_tty_prints_tool_commands_instead_of_running_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let bin = dir.path().join("bin");
    let fakes = dir.path().join("fakes");
    fs::create_dir_all(home.join("share/relaybook")).expect("mkdir");
    fs::create_dir_all(&fakes).expect("mkdir");
    let uv = fakes.join("uv");
    fs::write(&uv, "#!/bin/sh\necho called >> \"$UV_LOG\"\nexit 0\n").expect("write");
    chmod(&uv);
    let log = dir.path().join("uv.log");
    fs::create_dir_all(&bin).expect("mkdir");
    fs::write(bin.join("relaybook"), "#!/bin/sh\nexit 0\n").expect("binary");
    chmod(&bin.join("relaybook"));
    fs::write(
        home.join("share/relaybook/install.json"),
        format!(
            "{{\n  \"binary\": \"{}\",\n  \"graphify\": \"uv\",\n  \"rtk\": \"brew\",\n  \"rtk_init\": true\n}}\n",
            bin.join("relaybook").display()
        ),
    )
    .expect("manifest");
    script(&home, &bin)
        .env("PATH", format!("{}:/usr/bin:/bin", fakes.display()))
        .env("UV_LOG", &log)
        .args(["--uninstall"])
        .assert()
        .success()
        .stdout(predicates::str::contains("uv tool uninstall graphifyy"))
        .stdout(predicates::str::contains("brew uninstall rtk"))
        .stdout(predicates::str::contains("rtk init -g --uninstall"));
    assert!(!log.exists());
}
