use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The mod's `configDir` (mods/proxy-runs/hooks/register.tsx) must agree.
pub fn config_dir() -> PathBuf {
    if let Some(xdg) = env_dir("XDG_CONFIG_HOME") {
        return xdg.join("claude-proxy");
    }
    #[cfg(windows)]
    if let Some(appdata) = env_dir("APPDATA") {
        return appdata.join("claude-proxy");
    }
    home().join(".config").join("claude-proxy")
}

pub fn account_config_dir(label: &str) -> PathBuf {
    config_dir().join("accounts").join(label)
}

/// Claude Code's own home: `USERPROFILE` on Windows, where Git Bash also sets `HOME`.
pub fn home() -> PathBuf {
    let order = if cfg!(windows) {
        ["USERPROFILE", "HOME"]
    } else {
        ["HOME", "USERPROFILE"]
    };
    order
        .into_iter()
        .find_map(env_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `canonicalize` without Windows' `\\?\` prefix, which git and Claude do not take.
pub fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let path = path.canonicalize()?;
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{unc}")));
        }
        if let Some(local) = text.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(local));
        }
    }
    Ok(path)
}

/// The program to start for `name` (e.g. `claude`). On Windows that may be npm's
/// `claude.cmd`, which `Command::new` would not find: it only adds `.exe`.
pub fn program(name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            for ext in exts.split(';').filter(|e| !e.is_empty()) {
                let candidate = dir.join(format!("{name}{}", ext.to_ascii_lowercase()));
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    PathBuf::from(name)
}

/// A run or Claude session id that is safe as one file name under the config dir.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    static WRITES: AtomicUsize = AtomicUsize::new(0);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    // A leading `.` and a `.tmp` end: inbox readers skip dot-files and
    // asks::pending only takes `*.json`.
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut f = options.open(&tmp)?;
    let result = f.write_all(bytes).and_then(|()| {
        drop(f);
        rename(&tmp, path)
    });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// On Windows a rename onto a file another rename or reader holds open fails
/// for a moment with access denied.
fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Err(e)
                if cfg!(windows)
                    && e.kind() == std::io::ErrorKind::PermissionDenied
                    && tries < 50 =>
            {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            result => return result,
        }
    }
}

pub fn append(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Also creates or tightens the claude-proxy dirs above `dir`; nothing above the
/// config dir is touched.
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    let root = config_dir();
    let mut chain: Vec<&Path> = dir.ancestors().filter(|p| p.starts_with(&root)).collect();
    if chain.is_empty() {
        chain.push(dir);
    }
    chain.reverse();
    if let Some(parent) = chain[0].parent() {
        fs::create_dir_all(parent)?;
    }
    for p in chain {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            match fs::DirBuilder::new().mode(0o700).create(p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if fs::metadata(p)?.permissions().mode() & 0o077 != 0 {
                        // Best effort: a filesystem without Unix modes must not stop runs.
                        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        #[cfg(not(unix))]
        fs::create_dir_all(p)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_atomic_write_replaces_the_file_and_leaves_no_temp() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("f.json");
        write_atomic(&p, b"one", 0o600).unwrap();
        write_atomic(&p, b"two", 0o600).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(fs::read_dir(t.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn concurrent_atomic_writes_never_tear() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("f");
        let payloads: Vec<Vec<u8>> = (0..8u8).map(|i| vec![b'a' + i; 4096]).collect();
        let writers: Vec<_> = payloads
            .iter()
            .cloned()
            .map(|bytes| {
                let p = p.clone();
                std::thread::spawn(move || write_atomic(&p, &bytes, 0o600))
            })
            .collect();
        for w in writers {
            w.join().unwrap().unwrap();
        }
        assert!(payloads.contains(&fs::read(&p).unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn private_dir_creates_and_tightens() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let fresh = t.path().join("fresh");
        private_dir(&fresh).unwrap();
        assert_eq!(mode(&fresh), 0o700);
        let old = t.path().join("old");
        fs::create_dir(&old).unwrap();
        fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&old).unwrap();
        assert_eq!(mode(&old), 0o700);
    }

    #[test]
    fn program_falls_back_to_the_bare_name() {
        assert_eq!(
            program("claude-proxy-no-such-program"),
            PathBuf::from("claude-proxy-no-such-program")
        );
    }

    #[test]
    fn ids_cannot_escape_their_dir() {
        for good in ["a1b2c3", "7ecce39f-0ebf-43d2-bdff-c0fa9272d4b0"] {
            assert!(valid_id(good), "{good:?}");
        }
        let long = "a".repeat(65);
        for bad in ["", "../x", "a/b", "a.b", "a b", long.as_str()] {
            assert!(!valid_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn account_dir_is_under_the_config_dir_and_label_scoped() {
        let a = account_config_dir("personal");
        let b = account_config_dir("work");
        assert!(a.starts_with(config_dir()));
        assert!(a.ends_with(Path::new("accounts").join("personal")));
        assert_ne!(a, b);
    }
}
