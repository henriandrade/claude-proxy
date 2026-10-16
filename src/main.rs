//! One binary, two roles by `argv[0]` (busybox-style): `claude-proxy` is the manager;
//! any other name (an installed proxy like `claude-personal`) runs `claude` on that account.

mod add;
mod agents;
mod asks;
mod cli;
mod creds;
mod drainer;
mod hook;
mod install;
mod mcp;
mod paths;
mod proxy;
mod quota;
mod registry;
mod render;
mod runs;
mod shared;
#[cfg(windows)]
mod win;

use std::ffi::OsString;
use std::path::PathBuf;

const MANAGER_NAME: &str = "claude-proxy";

fn main() {
    // `claude-proxy status x | head` closes stdout early: end quietly, as the
    // signal would, instead of panicking in `println!`.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info.payload().downcast_ref::<String>();
        if message.is_some_and(|m| m.contains("Broken pipe")) {
            std::process::exit(141);
        }
        default_hook(info);
    }));

    let mut raw: Vec<OsString> = std::env::args_os().collect();
    let argv0 = raw.first().cloned().unwrap_or_default();
    let invoked = basename(&argv0);

    if invoked != MANAGER_NAME {
        let args: Vec<OsString> = raw.split_off(1);
        match proxy::run(&invoked, &args) {
            Ok(code) => std::process::exit(code),
            Err(message) => {
                eprintln!("{invoked}: {message}");
                std::process::exit(1);
            }
        }
    }

    std::process::exit(cli::main());
}

/// Only `.exe` comes off: a label may hold dots (`acct.2`).
fn basename(arg: &OsString) -> String {
    let Some(name) = PathBuf::from(arg)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
    else {
        return MANAGER_NAME.to_string();
    };
    match name.len().checked_sub(4) {
        Some(cut) if name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(".exe") => {
            name[..cut].to_string()
        }
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_of(arg: &str) -> String {
        basename(&OsString::from(arg))
    }

    #[test]
    fn basename_is_the_invoked_command_not_the_path() {
        assert_eq!(name_of("/usr/local/bin/claude-proxy"), MANAGER_NAME);
        assert_eq!(name_of("claude-proxy"), MANAGER_NAME);
        assert_eq!(name_of("./target/release/claude-proxy"), MANAGER_NAME);
        assert_eq!(
            name_of("/home/me/.local/bin/claude-personal"),
            "claude-personal"
        );
        assert_eq!(name_of("claude-personal.exe"), "claude-personal");
        assert_eq!(name_of("CLAUDE-PROXY.EXE"), "CLAUDE-PROXY");
        assert_eq!(name_of("/home/me/.local/bin/acct.2"), "acct.2");
    }
}
