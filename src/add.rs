//! Claude's own login runs in the new profile and stores its own credentials;
//! claude-proxy never handles the token.

use std::path::Path;
use std::process::Command;

use crate::install;
use crate::paths::account_config_dir;
use crate::registry::{valid_label, Registry};

pub struct Added {
    pub label: String,
    pub bin_path: std::path::PathBuf,
    pub on_path: bool,
}

pub fn run(
    self_exe: &Path,
    bin_dir: &Path,
    label: &str,
    interactive: bool,
) -> Result<Added, String> {
    if !valid_label(label) {
        return Err(format!(
            "invalid label {label:?}: use letters, digits, and . _ - (starting alphanumeric)"
        ));
    }
    let mut registry = Registry::load();
    if registry.has(label) {
        return Err(format!(
            "{label:?} already exists. Remove it first:  claude-proxy remove {label}"
        ));
    }
    if !interactive {
        return Err(
            "adding an account needs an interactive terminal: it signs in through Claude's own \
             browser login. Run `claude-proxy add` directly in your shell."
                .into(),
        );
    }

    let dir = account_config_dir(label);
    crate::paths::private_dir(&dir)
        .map_err(|e| format!("could not create the profile dir for {label:?}: {e}"))?;
    crate::shared::link_shared(&dir);
    seed_profile(&dir);

    eprintln!("\nSigning in {label:?} — opening Claude's login.");
    eprintln!("Log in with the account you want {label:?} to use (its own browser session),");
    eprintln!("then type /exit (or press Ctrl-C) to finish.\n");

    // An inherited credential would short-circuit the login.
    let status = Command::new("claude")
        .env("CLAUDE_CONFIG_DIR", &dir)
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "`claude` is not on PATH — install Claude Code first".to_string()
            } else {
                format!("could not run `claude`: {e}")
            }
        })?;
    // A Ctrl-C exit is fine; the login is verified below.
    let _ = status;

    if !profile_is_logged_in(&dir) {
        return Err(format!(
            "no login was completed for {label:?} — nothing was installed. \
             Run `claude-proxy add {label}` again and finish the sign-in."
        ));
    }

    let bin_path = install::install(self_exe, bin_dir, label)
        .map_err(|e| format!("could not install the {label:?} command: {e}"))?;
    registry.add(label);
    registry
        .save()
        .map_err(|e| format!("signed in but could not update the registry: {e}"))?;

    Ok(Added {
        on_path: install::on_path(bin_dir),
        label: label.to_string(),
        bin_path,
    })
}

fn profile_is_logged_in(dir: &Path) -> bool {
    let text = match std::fs::read_to_string(dir.join(".claude.json")) {
        Ok(t) => t,
        Err(_) => return false,
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| {
            v.get("oauthAccount")
                .and_then(|a| a.get("accountUuid"))
                .and_then(|u| u.as_str())
                .map(|u| !u.trim().is_empty())
        })
        .unwrap_or(false)
}

/// Skips the theme/onboarding prompt before sign-in. Best-effort.
fn seed_profile(dir: &Path) {
    let path = dir.join(".claude.json");
    if path.exists() {
        return;
    }
    let seed = serde_json::json!({
        "theme": "dark",
        "hasCompletedOnboarding": true,
    });
    if let Ok(bytes) = serde_json::to_vec_pretty(&seed) {
        let _ = std::fs::write(path, bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logged_in_only_when_an_account_uuid_is_present() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!profile_is_logged_in(dir.path()));
        std::fs::write(dir.path().join(".claude.json"), r#"{"theme":"dark"}"#).unwrap();
        assert!(!profile_is_logged_in(dir.path()));
        std::fs::write(
            dir.path().join(".claude.json"),
            r#"{"oauthAccount":{"accountUuid":"abc-123"}}"#,
        )
        .unwrap();
        assert!(profile_is_logged_in(dir.path()));
    }

    #[test]
    fn seed_profile_writes_once_and_does_not_clobber() {
        let dir = tempfile::tempdir().unwrap();
        seed_profile(dir.path());
        let first = std::fs::read_to_string(dir.path().join(".claude.json")).unwrap();
        assert!(first.contains("\"theme\""));
        // A second call must not overwrite an existing profile.
        std::fs::write(dir.path().join(".claude.json"), r#"{"real":"data"}"#).unwrap();
        seed_profile(dir.path());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".claude.json")).unwrap(),
            r#"{"real":"data"}"#
        );
    }
}
