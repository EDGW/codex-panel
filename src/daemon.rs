//! Shared Codex daemon readiness and best-effort cleanup after our proxy disconnects.
use crate::AppResult;
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

fn version(codex: &OsStr) -> AppResult<Value> {
    let output = Command::new(codex)
        .args(["app-server", "daemon", "version"])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "Could not inspect shared Codex daemon: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

pub(crate) fn shared_socket() -> AppResult<String> {
    shared_socket_with(OsStr::new("codex"), Duration::from_secs(10))
}

fn ready_socket(codex: &OsStr) -> AppResult<String> {
    let daemon = version(codex)?;
    if daemon.get("status").and_then(Value::as_str) != Some("running") {
        return Err("Shared Codex daemon is not running".into());
    }
    let socket = daemon
        .get("socketPath")
        .and_then(Value::as_str)
        .ok_or("No shared daemon socket path")?;
    // A reported running process may still have a stale or not-yet-listening socket.
    UnixStream::connect(socket)
        .map_err(|error| format!("Shared Codex daemon socket {socket}: {error}"))?;
    Ok(socket.to_owned())
}

fn shared_socket_with(codex: &OsStr, timeout: Duration) -> AppResult<String> {
    if let Ok(socket) = ready_socket(codex) {
        return Ok(socket);
    }
    // `version` exits unsuccessfully when the daemon is absent, rather than
    // always returning a JSON stopped status. `start` is idempotent.
    let started = Command::new(codex)
        .args(["app-server", "daemon", "start"])
        .output()?;
    if !started.status.success() {
        return Err(format!(
            "Could not start shared Codex daemon: {}",
            String::from_utf8_lossy(&started.stderr).trim()
        )
        .into());
    }
    let deadline = Instant::now() + timeout;
    loop {
        match ready_socket(codex) {
            Ok(socket) => return Ok(socket),
            Err(error) if Instant::now() >= deadline => {
                return Err(format!("Shared Codex daemon did not become ready: {error}").into());
            }
            Err(_) => std::thread::sleep(
                Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
            ),
        }
    }
}

/// Unavailable or ambiguous state must never cause the shared daemon to be stopped.
pub(crate) fn stop_if_idle(socket: &str) {
    if let Err(error) = inspect_and_stop(socket, OsStr::new("codex"), OsStr::new("lsof")) {
        eprintln!("codex-panel: keeping shared app-server: {error}");
    }
}

fn local_socket(info: &Value) -> Option<PathBuf> {
    if info.get("status")?.as_str()? != "running" || info.get("backend")?.as_str()? != "pid" {
        return None;
    }
    std::fs::canonicalize(info.get("socketPath")?.as_str()?).ok()
}

fn inspect_and_stop(socket: &str, codex: &OsStr, lsof: &OsStr) -> AppResult<()> {
    // On macOS lsof reports both the listening and accepted Unix sockets by path.
    // Other platforms need their own connection inspection before enabling cleanup.
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let expected = std::fs::canonicalize(socket)?;
    if local_socket(&version(codex)?).as_ref() != Some(&expected) {
        return Ok(());
    }
    let stream = UnixStream::connect(&expected)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let (mut probe, _) = tungstenite::client("ws://localhost/rpc", stream)?;
    rpc(
        &mut probe,
        1,
        "initialize",
        json!({"clientInfo":{"name":"cc-panel-cleanup","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}),
    )?;
    probe.send(Message::Text(
        json!({"method":"initialized"}).to_string().into(),
    ))?;
    let mut pid = None;
    // Give disconnects time to settle, then repeat immediately before stopping.
    for pass in 0..2 {
        if pass > 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        let diagnostics = rpc(&mut probe, 2 + pass * 2, "server/diagnostics", json!({}))?;
        let remote = rpc(
            &mut probe,
            3 + pass * 2,
            "remoteControl/status/read",
            Value::Null,
        )?;
        if !idle_work(&diagnostics)
            || remote.get("status").and_then(Value::as_str) != Some("disabled")
        {
            return Ok(());
        }
        let current_pid = diagnostics["process"]["id"]
            .as_u64()
            .filter(|id| *id > 0 && *id <= i32::MAX as u64)
            .ok_or("No daemon process identity in diagnostics")?;
        if pid.is_some_and(|previous| previous != current_pid) {
            return Ok(());
        }
        pid = Some(current_pid);
        if local_socket(&version(codex)?).as_ref() != Some(&expected) {
            return Ok(());
        }
        let output = Command::new(lsof)
            .args([
                "-n",
                "-P",
                "-a",
                "-p",
                &current_pid.to_string(),
                "-U",
                "-F",
                "fn",
            ])
            .output()?;
        if !output.status.success() {
            return Err("Could not inspect daemon client connections with lsof".into());
        }
        let listing = std::str::from_utf8(&output.stdout)?;
        // One listening socket plus our diagnostic connection. Any other client
        // (including a handshake not yet initialized) keeps the daemon alive.
        if !only_probe_connected(listing, current_pid, &expected) {
            return Ok(());
        }
    }
    // Codex currently has no atomic stop-if-idle operation. This remains a
    // best-effort snapshot; a new client can arrive between inspection and stop.
    let output = Command::new(codex)
        .args(["app-server", "daemon", "stop"])
        .output()?;
    if !output.status.success() {
        return Err("Could not stop idle shared Codex daemon".into());
    }
    Ok(())
}

fn idle_work(diagnostics: &Value) -> bool {
    let Some(gauges) = diagnostics.get("gauges").and_then(Value::as_array) else {
        return false;
    };
    let mut values = std::collections::HashMap::new();
    for gauge in gauges {
        let (Some(name), Some(value)) = (
            gauge.get("name").and_then(Value::as_str),
            gauge.get("value").and_then(Value::as_u64),
        ) else {
            return false;
        };
        if values.insert(name, value).is_some() {
            return false;
        }
    }
    // Codex returns registered gauges, not a fixed set: a counter that has
    // never been used is absent. The diagnostic request itself must still be
    // counted, so an empty or unrecognized diagnostic snapshot is not idle.
    if values.get("app.requests.in_flight") != Some(&1) {
        return false;
    }
    let gauge = |name: &str| values.get(name).copied().unwrap_or(0);
    gauge("core.turns.active") == 0
        && gauge("app.requests.queued") == 0
        && gauge("app.server_requests.pending") == 0
}

fn only_probe_connected(listing: &str, pid: u64, path: &Path) -> bool {
    let Some(path) = path.to_str() else {
        return false;
    };
    let mut lines = listing.lines();
    if lines.next() != Some(format!("p{pid}").as_str()) {
        return false;
    }
    let mut sockets = 0;
    let mut has_fd = false;
    for line in lines {
        if let Some(fd) = line.strip_prefix('f') {
            if has_fd {
                return false;
            }
            has_fd = !fd.is_empty() && fd.bytes().all(|byte| byte.is_ascii_digit());
            if !has_fd {
                return false;
            }
        } else if let Some(name) = line.strip_prefix('n') {
            if !has_fd {
                return false;
            }
            if name == path {
                sockets += 1;
            }
            has_fd = false;
        } else {
            return false;
        }
    }
    sockets == 2 && !has_fd
}

fn rpc(
    probe: &mut WebSocket<UnixStream>,
    id: u64,
    method: &str,
    params: Value,
) -> AppResult<Value> {
    probe.send(Message::Text(
        json!({"id":id,"method":method,"params":params})
            .to_string()
            .into(),
    ))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match probe.read()? {
            Message::Text(text) => {
                let message: Value = serde_json::from_str(text.as_str())?;
                if message.get("id").and_then(Value::as_u64) == Some(id) {
                    return message.get("result").cloned().ok_or_else(|| {
                        format!("Daemon does not support cleanup check {method}").into()
                    });
                }
            }
            Message::Close(_) => return Err("Daemon disconnected during cleanup check".into()),
            _ => {}
        }
    }
    Err("Daemon cleanup check timed out".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StartupFixture {
        runtime: crate::runtime::RuntimeDir,
        codex: PathBuf,
        socket: PathBuf,
    }

    impl StartupFixture {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let runtime = crate::runtime::RuntimeDir::create().unwrap();
            let codex = runtime.path().join("codex");
            let socket = runtime.path().join("daemon.sock");
            std::fs::write(
                &codex,
                r#"#!/bin/sh
cd -- "$(dirname -- "$0")" || exit 1
case "$3" in
version)
    if [ -f version.json ]; then cat version.json; else
        echo 'Connection refused (os error 61)' >&2; exit 1
    fi ;;
start)
    touch started
    if [ -f start-error ]; then cat start-error >&2; exit 1; fi
    if [ -f next-version.json ]; then cp next-version.json version.json; fi ;;
*) exit 1 ;;
esac
"#,
            )
            .unwrap();
            std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                runtime,
                codex,
                socket,
            }
        }

        fn write_version(&self, filename: &str, status: &str) {
            std::fs::write(
                self.runtime.path().join(filename),
                json!({"status":status,"socketPath":self.socket}).to_string(),
            )
            .unwrap();
        }
    }

    #[test]
    fn absent_stopped_and_stale_daemons_start_and_wait_for_a_listening_socket() {
        use std::os::unix::net::UnixListener;
        for initial_status in [None, Some("stopped"), Some("running")] {
            let fixture = StartupFixture::new();
            if let Some(status) = initial_status {
                fixture.write_version("version.json", status);
            }
            fixture.write_version("next-version.json", "running");
            let started = fixture.runtime.path().join("started");
            let socket = fixture.socket.clone();
            let server = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !started.exists() {
                    assert!(Instant::now() < deadline, "daemon start was never called");
                    std::thread::sleep(Duration::from_millis(10));
                }
                // start/version report success before the socket is available.
                std::thread::sleep(Duration::from_millis(150));
                UnixListener::bind(socket).unwrap()
            });
            let socket =
                shared_socket_with(fixture.codex.as_os_str(), Duration::from_secs(2)).unwrap();
            assert_eq!(socket, fixture.socket.to_str().unwrap());
            assert!(fixture.runtime.path().join("started").exists());
            drop(server.join().unwrap());
        }
    }

    #[test]
    fn an_already_listening_daemon_is_reused_without_starting() {
        let fixture = StartupFixture::new();
        let _listener = std::os::unix::net::UnixListener::bind(&fixture.socket).unwrap();
        fixture.write_version("version.json", "running");
        assert_eq!(
            shared_socket_with(fixture.codex.as_os_str(), Duration::ZERO).unwrap(),
            fixture.socket.to_str().unwrap()
        );
        assert!(!fixture.runtime.path().join("started").exists());
    }

    #[test]
    fn daemon_start_failure_reports_the_command_error() {
        let fixture = StartupFixture::new();
        std::fs::write(fixture.runtime.path().join("start-error"), "startup denied").unwrap();
        let error = shared_socket_with(fixture.codex.as_os_str(), Duration::ZERO)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Could not start shared Codex daemon"),
            "{error}"
        );
        assert!(error.contains("startup denied"), "{error}");
    }

    #[test]
    fn a_daemon_that_never_listens_fails_with_socket_context() {
        let fixture = StartupFixture::new();
        fixture.write_version("next-version.json", "running");
        let error = shared_socket_with(fixture.codex.as_os_str(), Duration::from_millis(100))
            .unwrap_err()
            .to_string();
        assert!(error.contains("did not become ready"), "{error}");
        assert!(error.contains(fixture.socket.to_str().unwrap()), "{error}");
    }

    #[test]
    fn other_clients_and_incomplete_connection_observations_keep_daemon_running() {
        let path = Path::new("/tmp/daemon.sock");
        let idle = "p42\nf10\nn/tmp/daemon.sock\nf20\nn/tmp/daemon.sock\nf7\nn->0x123\n";
        assert!(only_probe_connected(idle, 42, path));
        assert!(!only_probe_connected(
            &format!("{idle}f21\nn/tmp/daemon.sock\n"),
            42,
            path
        ));
        assert!(!only_probe_connected(idle, 43, path));
        assert!(!only_probe_connected(
            "p42\nf10\nn/tmp/daemon.sock\n",
            42,
            path
        ));
        assert!(!only_probe_connected(&format!("{idle}f22\n"), 42, path));
        assert!(!only_probe_connected("", 42, path));
    }

    #[test]
    fn running_or_unknown_work_keeps_daemon_running() {
        let names = [
            "core.turns.active",
            "app.requests.queued",
            "app.server_requests.pending",
            "app.requests.in_flight",
        ];
        let idle = json!({"gauges":names.iter().map(|name| json!({"name":name,"value":u64::from(*name == "app.requests.in_flight")})).collect::<Vec<_>>()});
        assert!(idle_work(&idle));
        for index in 0..names.len() {
            let mut busy = idle.clone();
            busy["gauges"][index]["value"] =
                json!(busy["gauges"][index]["value"].as_u64().unwrap() + 1);
            assert!(!idle_work(&busy));
            let mut unknown = idle.clone();
            unknown["gauges"].as_array_mut().unwrap().remove(index);
            assert_eq!(
                idle_work(&unknown),
                names[index] != "app.requests.in_flight"
            );
        }
        assert!(!idle_work(&json!({})));
        assert!(!idle_work(&json!({"gauges":[]})));
        let mut invalid = idle.clone();
        invalid["gauges"][0]["value"] = json!("unknown");
        assert!(!idle_work(&invalid));
        let duplicate = idle["gauges"][0].clone();
        invalid = idle.clone();
        invalid["gauges"].as_array_mut().unwrap().push(duplicate);
        assert!(!idle_work(&invalid));
        assert!(idle_work(
            &json!({"gauges":[{"name":"app.requests.in_flight","value":1}]})
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cleanup_stops_only_idle_daemon_and_retains_clients_work_remote_and_unknown_state() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;
        use std::time::{SystemTime, UNIX_EPOCH};

        for (clients, turns, remote, unknown, stop_expected) in [
            (0, 0, "disabled", false, true),
            (1, 0, "disabled", false, false),
            (0, 1, "disabled", false, false),
            (0, 0, "connected", false, false),
            (0, 0, "disabled", true, false),
        ] {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            // macOS Unix socket paths must fit in 104 bytes.
            let root =
                Path::new("/tmp").join(format!("ccp-cleanup-{}-{stamp}", std::process::id()));
            std::fs::create_dir(&root).unwrap();
            let root = std::fs::canonicalize(root).unwrap();
            let socket = root.join("daemon.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let codex = root.join("codex");
            let lsof = root.join("lsof");
            std::fs::write(&codex, "#!/bin/sh\ncd -- \"$(dirname -- \"$0\")\" || exit 1\ncase \"$3\" in\nversion) cat version.json ;;\nstop) touch stopped ;;\n*) exit 1 ;;\nesac\n").unwrap();
            std::fs::write(
                &lsof,
                "#!/bin/sh\ncd -- \"$(dirname -- \"$0\")\" || exit 1\ncat sockets.txt\n",
            )
            .unwrap();
            for script in [&codex, &lsof] {
                std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            std::fs::write(
                root.join("version.json"),
                json!({"status":"running","backend":"pid","socketPath":socket}).to_string(),
            )
            .unwrap();
            let mut listing = "p42\n".to_owned();
            for index in 0..2 + clients {
                listing.push_str(&format!("f{}\nn{}\n", index + 10, socket.display()));
            }
            std::fs::write(root.join("sockets.txt"), listing).unwrap();
            let server = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut ws = tungstenite::accept(stream).unwrap();
                let mut diagnostic_calls = 0;
                while let Ok(Message::Text(text)) = ws.read() {
                    let request: Value = serde_json::from_str(text.as_str()).unwrap();
                    let Some(id) = request.get("id") else {
                        continue;
                    };
                    let result = match request["method"].as_str().unwrap() {
                        "initialize" => json!({}),
                        "server/diagnostics" => {
                            diagnostic_calls += 1;
                            json!({"process":{"id":42},"gauges":[
                                {"name":"core.turns.active","value":turns},
                                {"name":"app.requests.queued","value":0},
                                {"name":"app.server_requests.pending","value":0},
                                {"name":"app.requests.in_flight","value":1}
                            ]})
                        }
                        "remoteControl/status/read" => json!({"status":remote}),
                        other => panic!("Unexpected request: {other}"),
                    };
                    let response = if unknown && request["method"] == "server/diagnostics" {
                        json!({"id":id,"error":{"code":-32601,"message":"Unsupported"}})
                    } else {
                        json!({"id":id,"result":result})
                    };
                    ws.send(Message::Text(response.to_string().into())).unwrap();
                }
                diagnostic_calls
            });
            let result = inspect_and_stop(
                socket.to_str().unwrap(),
                codex.as_os_str(),
                lsof.as_os_str(),
            );
            assert_eq!(result.is_err(), unknown, "cleanup returned {result:?}");
            assert_eq!(root.join("stopped").exists(), stop_expected);
            assert_eq!(server.join().unwrap(), if stop_expected { 2 } else { 1 });
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
