//! An account's credentials, in the store Claude Code itself uses so a refreshed token is
//! the one the next `claude` run sees. Ported from ai-usagebar's `anthropic::keychain` / `creds`.

use std::path::Path;

pub enum Source {
    #[cfg(target_os = "macos")]
    Keychain { service: String },
    #[cfg(not(target_os = "macos"))]
    File(std::path::PathBuf),
}

/// Claude Code's own naming; `None` is the primary profile.
pub fn source_for(config_dir: Option<&Path>) -> Result<Source, String> {
    #[cfg(target_os = "macos")]
    {
        let service = match config_dir {
            None => "Claude Code-credentials".to_string(),
            Some(dir) => format!(
                "Claude Code-credentials-{}",
                sha256_prefix(&dir.to_string_lossy())?
            ),
        };
        Ok(Source::Keychain { service })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = config_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| crate::paths::home().join(".claude"));
        Ok(Source::File(dir.join(".credentials.json")))
    }
}

/// `None` when the account is not logged in.
pub fn read(source: &Source) -> Result<Option<String>, String> {
    match source {
        #[cfg(target_os = "macos")]
        Source::Keychain { service } => keychain::read(service),
        #[cfg(not(target_os = "macos"))]
        Source::File(path) => match std::fs::read_to_string(path) {
            Ok(s) if s.trim().is_empty() => Ok(None),
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("could not read {}: {e}", path.display())),
        },
    }
}

pub fn write(source: &Source, blob: &str) -> Result<(), String> {
    match source {
        #[cfg(target_os = "macos")]
        Source::Keychain { service } => keychain::write(service, blob),
        #[cfg(not(target_os = "macos"))]
        Source::File(path) => write_file_private(path, blob),
    }
}

#[cfg(not(target_os = "macos"))]
fn write_file_private(path: &Path, blob: &str) -> Result<(), String> {
    crate::paths::write_atomic(path, blob.as_bytes(), 0o600)
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

#[cfg(target_os = "macos")]
fn sha256_prefix(text: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run shasum: {e}"))?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(text.as_bytes())
        .map_err(|e| format!("could not run shasum: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("could not run shasum: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .and_then(|h| h.get(..8))
        .map(str::to_string)
        .ok_or_else(|| "shasum produced unexpected output".to_string())
}

/// Only through `/usr/bin/security`: a native write stamps this binary onto the item's
/// partition list, and every later read, Claude Code's too, raises a Keychain dialog.
#[cfg(target_os = "macos")]
mod keychain {
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// `security` exits with the raw OSStatus; 44 is `errSecItemNotFound`.
    const NOT_FOUND: i32 = 44;
    /// A margin under the undocumented `security -i` line cap. A longer line is
    /// silently truncated *and stored*, so anything bigger goes via argv.
    const STDIN_SAFE_MAX: usize = 4000;

    fn account() -> Option<String> {
        std::env::var("USER").ok().filter(|u| !u.is_empty())
    }

    pub fn read(service: &str) -> Result<Option<String>, String> {
        let mut cmd = Command::new("/usr/bin/security");
        cmd.args(["find-generic-password", "-s", service, "-w"]);
        if let Some(a) = account() {
            cmd.args(["-a", &a]);
        }
        let out = cmd
            .output()
            .map_err(|e| format!("could not run security: {e}"))?;
        if !out.status.success() {
            if out.status.code() == Some(NOT_FOUND) {
                return Ok(None);
            }
            return Err(format!(
                "could not read the Keychain credential (security exited {}); \
                 unlock the login Keychain and retry",
                out.status.code().unwrap_or(-1)
            ));
        }
        let value = String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_string();
        Ok((!value.is_empty()).then_some(value))
    }

    pub fn write(service: &str, blob: &str) -> Result<(), String> {
        // Fail closed without $USER: a write without `-a` could create a second
        // item that the read would never find.
        let account = account().ok_or("$USER is not set; refusing to write the Keychain")?;
        let out = match stdin_command(service, &account, blob) {
            Some(command) => {
                let mut child = Command::new("/usr/bin/security")
                    .arg("-i")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| format!("could not run security: {e}"))?;
                child
                    .stdin
                    .take()
                    .expect("stdin was piped")
                    .write_all(command.as_bytes())
                    .map_err(|e| format!("could not run security: {e}"))?;
                child.wait_with_output()
            }
            // Too long for `security -i`: argv, as Claude Code does.
            None => Command::new("/usr/bin/security")
                .args([
                    "add-generic-password",
                    "-U",
                    "-a",
                    &account,
                    "-s",
                    service,
                    "-w",
                    blob,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output(),
        }
        .map_err(|e| format!("could not run security: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "could not update the Keychain credential (security exited {})",
                out.status.code().unwrap_or(-1)
            ))
        }
    }

    /// `None` on a newline, which would end the `security -i` command early.
    fn quote(value: &str) -> Option<String> {
        if value.contains(['\n', '\r']) {
            return None;
        }
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for ch in value.chars() {
            if ch == '\\' || ch == '"' {
                out.push('\\');
            }
            out.push(ch);
        }
        out.push('"');
        Some(out)
    }

    pub(super) fn stdin_command(service: &str, account: &str, blob: &str) -> Option<String> {
        let command = format!(
            "add-generic-password -U -a {} -s {} -w {}\n",
            quote(account)?,
            quote(service)?,
            quote(blob)?,
        );
        (command.len() <= STDIN_SAFE_MAX).then_some(command)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::keychain::stdin_command;

    #[test]
    fn stdin_command_quotes_bounds_and_refuses_newlines() {
        let c = stdin_command("svc", "me", r#"{"a":"b\c"}"#).unwrap();
        assert_eq!(
            c,
            "add-generic-password -U -a \"me\" -s \"svc\" -w \"{\\\"a\\\":\\\"b\\\\c\\\"}\"\n"
        );
        assert!(stdin_command("svc", "me", "a\nb").is_none());
        assert!(stdin_command("svc", "me", &"x".repeat(5000)).is_none());
    }
}
