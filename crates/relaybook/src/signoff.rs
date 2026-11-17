//! Typed confirmation at a terminal, and the optional signed sign-off tag.
//!
//! The phrase is a verb plus 12 hex digits. Stdin and stdout must both be
//! terminals. The window starts once the prompt is visible and is judged
//! after `read_line`. `RELAYBOOK_SIGNOFF_TIMEOUT_SECS` overrides the 120
//! second default. This is a deliberate typed confirmation, not proof of a
//! human.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use relaybook_core::Error;

use crate::gitinfo;

pub(crate) const METHOD_TYPED: &str = "tty-typed";
pub(crate) const METHOD_SIGNED: &str = "tty-typed+signed-tag";
const DESCRIPTION: &str = "deliberate typed confirmation at a terminal; not proof of a human";

pub(crate) struct Challenge {
    pub ask: &'static str,
    pub verb: &'static str,
    pub token: String,
    pub missing_tty: &'static str,
    pub declined: &'static str,
}

pub(crate) fn confirm(challenge: &Challenge) -> Result<(), Error> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(Error::InvalidSetting(challenge.missing_tty.to_string()));
    }
    if challenge.token.len() != 12 || !challenge.token.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(Error::InvalidSetting(
            "confirmation requires a 12-digit token".to_string(),
        ));
    }
    let phrase = format!("{} {}", challenge.verb, challenge.token);
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{}", challenge.ask).map_err(|err| Error::InvalidSetting(err.to_string()))?;
    writeln!(stdout, "Type {phrase}").map_err(|err| Error::InvalidSetting(err.to_string()))?;
    writeln!(stdout, "{DESCRIPTION}").map_err(|err| Error::InvalidSetting(err.to_string()))?;
    stdout
        .flush()
        .map_err(|err| Error::InvalidSetting(err.to_string()))?;
    let started = Instant::now();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|err| Error::InvalidSetting(err.to_string()))?;
    if started.elapsed() > challenge_window() {
        return Err(Error::InvalidSetting(
            "confirmation challenge expired".to_string(),
        ));
    }
    if answer.trim() != phrase {
        return Err(Error::InvalidSetting(challenge.declined.to_string()));
    }
    Ok(())
}

/// First 12 hex digits of a commit, lowercased.
pub(crate) fn token_from_sha(sha: &str) -> Result<String, Error> {
    let sha = sha.trim();
    if sha.len() < 12 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::InvalidSetting(
            "confirmation requires a commit sha".to_string(),
        ));
    }
    Ok(sha[..12].to_ascii_lowercase())
}

/// `HEAD`'s first 12 hex digits, or a fresh 12-hex token when there is no commit.
pub(crate) fn confirmation_token(cwd: &Path) -> String {
    gitinfo::verified_head(cwd)
        .and_then(|sha| token_from_sha(&sha).ok())
        .unwrap_or_else(fresh_token)
}

/// Check annotated tag `relaybook-signoff/<sha12>` on `sha`.
///
/// The stored evidence is the tag name, the tag object SHA, and the first
/// verifier line. `git tag -v` is run with `LC_ALL=C` so that line does not
/// follow the operator locale. Relaybook does not create or push the tag.
pub(crate) fn verify_signoff_tag(root: &Path, sha: &str) -> Result<String, Error> {
    let sha = sha.trim();
    let name = format!("relaybook-signoff/{}", token_from_sha(sha)?);
    let Some(peeled) = git_rev(root, &format!("{name}^{{commit}}"))? else {
        return Err(Error::InvalidSetting(format!(
            "sign-off tag {name} is missing"
        )));
    };
    if peeled != sha {
        return Err(Error::InvalidSetting(format!(
            "sign-off tag {name} does not point at HEAD"
        )));
    }
    let verified = git_tag_verify(root, &name)?;
    if !verified.status.success() {
        return Err(Error::InvalidSetting(format!(
            "sign-off tag {name} is not a valid signature"
        )));
    }
    let object = git_rev(root, &format!("refs/tags/{name}"))?
        .ok_or_else(|| Error::InvalidSetting(format!("sign-off tag {name} is missing")))?;
    let verifier = first_line(&verified.stderr)
        .or_else(|| first_line(&verified.stdout))
        .unwrap_or_default();
    Ok(format!("tag {name}\nobject {object}\nverifier {verifier}"))
}

fn challenge_window() -> Duration {
    match std::env::var("RELAYBOOK_SIGNOFF_TIMEOUT_SECS") {
        Ok(value) => match value.parse::<u64>() {
            Ok(seconds) => Duration::from_secs(seconds),
            Err(_) => Duration::from_secs(120),
        },
        Err(_) => Duration::from_secs(120),
    }
}

fn fresh_token() -> String {
    static STEP: AtomicU64 = AtomicU64::new(1);
    let step = STEP.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mixed = (nanos ^ u128::from(step).wrapping_mul(0x9E37_79B9_7F4A_7C15))
        ^ (u128::from(std::process::id()) << 16);
    format!("{:012x}", mixed & 0x0000_FFFF_FFFF_FFFF)
}

fn git_cmd(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .env("LC_ALL", "C")
        .env("LANG", "C");
    command
}

fn git_rev(root: &Path, rev: &str) -> Result<Option<String>, Error> {
    let output = git_cmd(root)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options", rev])
        .output()
        .map_err(|err| Error::InvalidSetting(format!("git rev-parse failed: {err}")))?;
    match output.status.code() {
        Some(0) => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(text))
            }
        }
        Some(1) => Ok(None),
        Some(code) => Err(Error::InvalidSetting(format!(
            "git rev-parse failed: {code} {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
        None => Err(Error::InvalidSetting("git rev-parse failed".to_string())),
    }
}

fn git_tag_verify(root: &Path, name: &str) -> Result<Output, Error> {
    git_cmd(root)
        .args(["tag", "-v", "--end-of-options", name])
        .output()
        .map_err(|err| Error::InvalidSetting(format!("git tag failed: {err}")))
}

fn first_line(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}
