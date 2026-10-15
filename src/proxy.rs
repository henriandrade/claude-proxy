//! Claude keys its login and identity off `CLAUDE_CONFIG_DIR`, so the account's own dir is
//! the whole mechanism.

use std::ffi::OsString;
use std::path::Path;

use crate::paths::account_config_dir;

/// Credentials and a host app's (Claude Desktop, the Agent SDK) session identity; inherited,
/// they would tie the account's Claude to that identity.
const HOST_SESSION_VARS: &[&str] = &[
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_OAUTH_SCOPES",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_ACCOUNT_UUID",
    "CLAUDE_CODE_ORGANIZATION_UUID",
    "CLAUDE_CODE_USER_EMAIL",
    "CLAUDE_CODE_SDK_HAS_HOST_AUTH_REFRESH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_HOST_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
];

pub enum EnvOp {
    Set(&'static str, String),
    Unset(&'static str),
}

/// Inherited credentials are unset: they would silently override the account's own login.
/// The primary unsets `CLAUDE_CONFIG_DIR`, inherited when `auto` runs inside a proxy session.
pub fn proxy_env(config_dir: Option<&str>) -> Vec<EnvOp> {
    let mut ops = vec![match config_dir {
        Some(dir) => EnvOp::Set("CLAUDE_CONFIG_DIR", dir.to_string()),
        None => EnvOp::Unset("CLAUDE_CONFIG_DIR"),
    }];
    ops.extend(HOST_SESSION_VARS.iter().map(|k| EnvOp::Unset(k)));
    ops
}

pub fn run(label: &str, args: &[OsString]) -> Result<i32, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("no current directory: {e}"))?;
    let mut command = interactive_command(label, &cwd)?;
    command.args(args);
    exec_or_status(command, label)
}

pub fn run_primary(args: &[OsString]) -> Result<i32, String> {
    let mut command = claude_command(None);
    command.args(args);
    exec_or_status(command, crate::quota::PRIMARY_LABEL)
}

pub fn interactive_command(label: &str, cwd: &Path) -> Result<std::process::Command, String> {
    if label == crate::quota::PRIMARY_LABEL {
        return Ok(claude_command(None));
    }
    let config_dir = account_config_dir(label);
    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("could not create the config dir for {label:?}: {e}"))?;
    let mut command = claude_command(Some(&config_dir));
    let servers = crate::mcp::user_servers(cwd);
    if !servers.is_empty() {
        let file = crate::mcp::shared_file(&servers);
        crate::mcp::write(&file, servers)
            .map_err(|e| format!("could not write the MCP config: {e}"))?;
        command.arg(crate::mcp::flag(&file));
    }
    Ok(command)
}

pub fn claude_command(config_dir: Option<&Path>) -> std::process::Command {
    if let Some(dir) = config_dir {
        let _ = std::fs::create_dir_all(dir);
        crate::shared::link_shared(dir);
    }
    let dir = config_dir.map(|d| d.to_string_lossy().into_owned());
    let mut command = std::process::Command::new("claude");
    for op in proxy_env(dir.as_deref()) {
        match op {
            EnvOp::Set(k, v) => {
                command.env(k, v);
            }
            EnvOp::Unset(k) => {
                command.env_remove(k);
            }
        }
    }
    command
}

/// `exec` on Unix so signals and the exit code pass through exactly.
#[cfg(unix)]
fn exec_or_status(mut command: std::process::Command, _label: &str) -> Result<i32, String> {
    use std::os::unix::process::CommandExt;
    let err = command.exec();
    Err(classify_spawn_error(err))
}

#[cfg(not(unix))]
fn exec_or_status(mut command: std::process::Command, _label: &str) -> Result<i32, String> {
    match command.status() {
        Ok(status) => Ok(status.code().unwrap_or(1)),
        Err(err) => Err(classify_spawn_error(err)),
    }
}

fn classify_spawn_error(err: std::io::Error) -> String {
    if err.kind() == std::io::ErrorKind::NotFound {
        "`claude` is not on PATH — install Claude Code first".into()
    } else {
        format!("could not run `claude`: {err}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(
        ops: Vec<EnvOp>,
    ) -> (
        std::collections::HashMap<&'static str, String>,
        Vec<&'static str>,
    ) {
        let mut set = std::collections::HashMap::new();
        let mut unset = Vec::new();
        for op in ops {
            match op {
                EnvOp::Set(k, v) => {
                    set.insert(k, v);
                }
                EnvOp::Unset(k) => unset.push(k),
            }
        }
        (set, unset)
    }

    #[test]
    fn proxy_env_sets_the_config_dir_and_clears_inherited_credentials() {
        let (set, unset) = split(proxy_env(Some(
            "/home/me/.config/claude-proxy/accounts/personal",
        )));
        assert_eq!(
            set.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/home/me/.config/claude-proxy/accounts/personal")
        );
        for var in [
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "CLAUDE_CODE_ACCOUNT_UUID",
            "CLAUDE_CODE_SESSION_ID",
        ] {
            assert!(unset.contains(&var), "{var}");
        }
    }

    #[test]
    fn the_primary_profile_clears_an_inherited_config_dir() {
        let (set, unset) = split(proxy_env(None));
        assert!(!set.contains_key("CLAUDE_CONFIG_DIR"));
        assert!(unset.contains(&"CLAUDE_CONFIG_DIR"));
        assert!(unset.contains(&"CLAUDE_CODE_OAUTH_TOKEN"));
    }
}
