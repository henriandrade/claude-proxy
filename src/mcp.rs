//! `~/.claude.json` holds the login, so it cannot be shared with a profile; the primary's
//! MCP servers are passed with `--mcp-config` instead.

use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::paths::{config_dir, home};

pub fn user_servers(cwd: &Path) -> Map<String, Value> {
    servers_in(&home().join(".claude.json"), &cwd.to_string_lossy())
}

fn servers_in(claude_json: &Path, cwd: &str) -> Map<String, Value> {
    let Some(v) = fs::read(claude_json)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return Map::new();
    };
    let mut servers = v["mcpServers"].as_object().cloned().unwrap_or_default();
    if let Some(local) = v["projects"][cwd]["mcpServers"].as_object() {
        servers.extend(local.clone());
    }
    servers
}

/// The `=` form matters: Claude's flag is variadic and would swallow the next argument.
pub fn flag(file: &Path) -> String {
    format!("--mcp-config={}", file.display())
}

/// Owner-only: server entries can carry API keys.
pub fn write(file: &Path, servers: Map<String, Value>) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&json!({"mcpServers": servers}))?;
    if fs::read(file).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    if let Some(parent) = file.parent() {
        crate::paths::private_dir(parent)?;
    }
    crate::paths::write_atomic(file, &bytes, 0o600)
}

/// Named by its contents so launches from different directories never overwrite each other's.
pub fn shared_file(servers: &Map<String, Value>) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    Value::Object(servers.clone()).to_string().hash(&mut h);
    config_dir()
        .join("mcp")
        .join(format!("{:016x}.json", h.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_and_local_scope_servers_are_merged() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join(".claude.json");
        fs::write(
            &file,
            r#"{"mcpServers":{"a":{"command":"x"}},
                "projects":{"/repo":{"mcpServers":{"b":{"command":"y"}}},
                            "/other":{"mcpServers":{"c":{"command":"z"}}}}}"#,
        )
        .unwrap();
        let mut names: Vec<_> = servers_in(&file, "/repo").keys().cloned().collect();
        names.sort();
        assert_eq!(names, ["a", "b"]);
        assert!(servers_in(&t.path().join("missing"), "/repo").is_empty());
    }

    #[test]
    fn parallel_launches_can_write_the_same_config() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join("mcp/same.json");
        let mut servers = Map::new();
        servers.insert("a".into(), json!({"command": "x"}));
        let writers: Vec<_> = (0..8)
            .map(|_| {
                let (file, servers) = (file.clone(), servers.clone());
                std::thread::spawn(move || write(&file, servers))
            })
            .collect();
        for w in writers {
            w.join().unwrap().unwrap();
        }
        assert!(servers_in(&file, "/").contains_key("a"));
    }

    #[cfg(unix)]
    #[test]
    fn the_config_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join("mcp/x.json");
        write(&file, Map::new()).unwrap();
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
