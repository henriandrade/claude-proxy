use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Path::new(&xdg).join("claude-proxy");
        }
    }
    home().join(".config").join("claude-proxy")
}

pub fn account_config_dir(label: &str) -> PathBuf {
    config_dir().join("accounts").join(label)
}

pub fn home() -> PathBuf {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    if let Ok(up) = std::env::var("USERPROFILE") {
        if !up.is_empty() {
            return PathBuf::from(up);
        }
    }
    PathBuf::from(".")
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
    let result = f.write_all(bytes).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
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
        assert!(a.ends_with("accounts/personal"));
        assert_ne!(a, b);
    }
}
