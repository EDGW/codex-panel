//! Codex process lifecycle and native exit-output capture, independent of panel rendering.
use crate::{AppResult, bridge, runtime::RuntimeDir, session::Session, tmux::Tmux};
use std::ffi::OsString;
use std::os::unix::net::UnixListener;
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
    Ok(command(args).status()?)
}

pub(crate) fn run(
    runtime: &RuntimeDir,
    args: &[OsString],
    tmux: &Tmux,
    pane: &str,
) -> AppResult<ExitStatus> {
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
    let status = command(&[])
        .args(["--remote", &format!("unix://{}", socket.display())])
        .args(args)
        .status();
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
    Ok(status?)
}
