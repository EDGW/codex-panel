//! Codex process lifecycle and native exit-output capture, independent of panel rendering.
use crate::{AppResult, bridge, runtime::RuntimeDir, session::Session, subprocess, tmux::Tmux};
use std::ffi::OsString;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

pub(crate) const PROCESS_KIND: &str = "CC_PANEL_PROCESS_KIND";
pub(crate) const RUNTIME_DIR: &str = "CC_PANEL_RUNTIME_DIR";
pub(crate) const TMUX_SOCKET: &str = "CC_PANEL_TMUX_SOCKET";

fn command(args: &[OsString]) -> Command {
    let mut command = Command::new("codex");
    command
        .args(args)
        .env_remove(PROCESS_KIND)
        .env_remove(RUNTIME_DIR)
        .env_remove(TMUX_SOCKET);
    command
}

pub(crate) fn passthrough(args: &[OsString]) -> AppResult<ExitStatus> {
    subprocess::status(&mut command(args), "Could not start Codex CLI")
}

fn remote_command(mut command: Command, args: &[OsString], socket: &Path, cwd: &Path) -> Command {
    command
        .current_dir(cwd)
        .args(["--remote", &format!("unix://{}", socket.display())]);
    let explicit_directory = args
        .iter()
        .take_while(|arg| arg.as_os_str() != "--")
        .any(|arg| {
            let bytes = arg.as_encoded_bytes();
            bytes.starts_with(b"-C") || bytes == b"--cd" || bytes.starts_with(b"--cd=")
        });
    if !explicit_directory {
        // Remote sessions otherwise inherit the shared daemon's working directory.
        command.arg("--cd").arg(cwd);
    }
    command.args(args);
    command
}

pub(crate) fn run(
    runtime: &RuntimeDir,
    args: &[OsString],
    tmux: &Tmux,
    pane: &str,
) -> AppResult<ExitStatus> {
    let cwd = std::env::current_dir()?;
    // Fail before launching the TUI, and never let it connect while the daemon
    // is still starting. Preserve the underlying startup error for the user.
    let daemon_socket = crate::daemon::shared_socket()?;
    let socket = runtime.proxy_socket();
    let listener = UnixListener::bind(&socket)?;
    let stop = Arc::new(AtomicBool::new(false));
    let bridge_stop = stop.clone();
    let directory = runtime.path().to_owned();
    let worker = thread::spawn(move || {
        if let Err(error) = bridge::serve(listener, &daemon_socket, &directory, bridge_stop) {
            let _ = RuntimeDir::open(directory).save_session(&Session {
                error: Some(format!("connection error: {error}")),
                ..Default::default()
            });
        }
    });
    let status = subprocess::status(
        &mut remote_command(command(&[]), args, &socket, &cwd),
        "Could not start Codex CLI",
    );
    stop.store(true, Ordering::Relaxed);
    let _ = worker.join();
    if let Ok(output) = tmux.capture(pane) {
        // The proxy disappears on exit; native reconnect commands can use the shared daemon.
        let output = output.replace(
            &format!("codex --remote unix://{} ", socket.display()),
            "codex ",
        );
        runtime.save_exit(&output)?;
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(command: Command, args: &[OsString], cwd: &Path) -> Vec<u8> {
        let output = remote_command(command, args, Path::new("/tmp/panel proxy.sock"), cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        output.stdout
    }

    // Interpret directory options at the process boundary, including duplicate detection.
    fn fixture() -> Command {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            r#"workspace=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --remote) shift 2; continue ;;
        --cd|-C) directory=$2; shift 2 ;;
        --cd=*) directory=${1#--cd=}; shift ;;
        -C*) directory=${1#-C}; directory=${directory#=}; shift ;;
        --) shift; break ;;
        *) break ;;
    esac
    [ -z "$workspace" ] || exit 64
    workspace=$directory
done
[ -n "$workspace" ] || exit 65
cd -- "$workspace" || exit 66
printf '%s\0' "$(pwd -P)" "$@"
"#,
            "codex",
        ]);
        command
    }

    #[test]
    fn remote_session_defaults_to_caller_directory_and_preserves_literal_prompt() {
        let runtime = RuntimeDir::create().unwrap();
        let cwd = runtime.path().join("项目 with spaces 'quotes' $(literal)");
        std::fs::create_dir(&cwd).unwrap();
        // A directory-looking token after -- belongs to the prompt.
        let args = ["--", "--cd", "literal $(prompt)"].map(OsString::from);
        let output = workspace(fixture(), &args, &cwd);
        let expected = format!(
            "{}\0--cd\0literal $(prompt)\0",
            cwd.canonicalize().unwrap().display()
        );
        assert_eq!(output, expected.as_bytes());
    }

    #[test]
    fn explicit_directory_options_override_the_default_without_duplicates() {
        let runtime = RuntimeDir::create().unwrap();
        let cwd = runtime.path();
        let chosen = cwd.join("chosen directory");
        std::fs::create_dir(&chosen).unwrap();
        for args in [
            vec!["-C", "chosen directory"],
            vec!["--cd", "chosen directory"],
            vec!["-Cchosen directory"],
            vec!["-C=chosen directory"],
            vec!["--cd=chosen directory"],
        ] {
            let mut args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
            args.push("literal prompt".into());
            let output = workspace(fixture(), &args, cwd);
            let expected = format!(
                "{}\0literal prompt\0",
                chosen.canonicalize().unwrap().display()
            );
            assert_eq!(output, expected.as_bytes());
        }
    }
}
