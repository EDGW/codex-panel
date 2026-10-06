use crate::AppResult;
use crate::runtime::RuntimeDir;
use crate::session::{Tracker, selects_visible_thread};
use serde_json::{Value, json};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::{Error, Message};

fn blocked(error: &Error) -> bool {
    matches!(error, Error::Io(e) if e.kind() == std::io::ErrorKind::WouldBlock)
}

pub fn serve(
    listener: UnixListener,
    daemon_socket: &str,
    directory: &Path,
    stop: Arc<AtomicBool>,
) -> AppResult<()> {
    listener.set_nonblocking(true)?;
    let tracker = Arc::new(Mutex::new(Tracker::default()));
    let mut workers: Vec<thread::JoinHandle<()>> = Vec::new();
    let mut connection_id = 0;
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                connection_id += 1;
                let directory = directory.to_owned();
                let daemon_socket = daemon_socket.to_owned();
                let stop = stop.clone();
                let tracker = tracker.clone();
                workers.push(thread::spawn(move || {
                    if let Err(error) = connection(
                        stream,
                        &daemon_socket,
                        &directory,
                        &stop,
                        &tracker,
                        connection_id,
                    ) {
                        RuntimeDir::open(directory.clone()).save_bridge_error(&error.to_string());
                    }
                    if let Ok(mut tracker) = tracker.lock() {
                        tracker.disconnected(connection_id);
                    }
                }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                for worker in workers {
                    let _ = worker.join();
                }
                crate::daemon::stop_if_idle(daemon_socket);
                return Err(error.into());
            }
        }
        // Picker connections are short lived; reap them without waiting for live TUI connections.
        let mut index = 0;
        while index < workers.len() {
            if workers[index].is_finished() {
                let _ = workers.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
    crate::daemon::stop_if_idle(daemon_socket);
    Ok(())
}

fn connection(
    stream: UnixStream,
    path: &str,
    directory: &Path,
    stop: &AtomicBool,
    tracker: &Mutex<Tracker>,
    connection_id: u64,
) -> AppResult<()> {
    // Accepted sockets can inherit O_NONBLOCK on macOS. Complete the handshake
    // in blocking mode before switching to the nonblocking forwarding loop.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut websocket = tungstenite::accept(stream)?;
    websocket.get_mut().set_nonblocking(true)?;
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let (mut backend, _) = tungstenite::client("ws://localhost/rpc", stream)?;
    backend.get_mut().set_nonblocking(true)?;
    let result = (|| -> AppResult<()> {
        let mut selected_on_connection = false;
        let mut read_id: Option<String> = None;
        let mut counter = 0u64;
        let mut last_read = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            loop {
                match websocket.read() {
                    Ok(Message::Text(text)) => {
                        let message: Value = serde_json::from_str(text.as_str())?;
                        selected_on_connection |= selects_visible_thread(&message);
                        tracker
                            .lock()
                            .map_err(|_| "Session tracker lock poisoned")?
                            .request(connection_id, &message);
                        if let Err(error) = backend.send(Message::Text(text))
                            && !blocked(&error)
                        {
                            return Err(error.into());
                        }
                    }
                    Ok(Message::Close(_)) => return Ok(()),
                    Ok(_) => {}
                    Err(error) if blocked(&error) => break,
                    Err(Error::ConnectionClosed | Error::AlreadyClosed) => return Ok(()),
                    Err(error) => return Err(error.into()),
                }
            }
            loop {
                match backend.read() {
                    Ok(Message::Text(text)) => {
                        let message: Value = serde_json::from_str(text.as_str())?;
                        {
                            let mut tracker = tracker
                                .lock()
                                .map_err(|_| "Session tracker lock poisoned")?;
                            let before = tracker.current.clone();
                            tracker.response(connection_id, &message);
                            if tracker.current != before {
                                RuntimeDir::open(directory.to_owned())
                                    .save_session(&tracker.current)?;
                            }
                        }
                        let internal = message
                            .get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| read_id.as_deref() == Some(id));
                        if internal {
                            read_id = None;
                        } else if let Err(error) = websocket.send(Message::Text(text))
                            && !blocked(&error)
                        {
                            return Err(error.into());
                        }
                    }
                    Ok(Message::Close(_)) => {
                        return Err("Shared app-server connection closed".into());
                    }
                    Ok(_) => {}
                    Err(error) if blocked(&error) => break,
                    Err(error) => return Err(error.into()),
                }
            }
            // Refresh metadata through this connection; never guess from global recency.
            if selected_on_connection
                && read_id.is_none()
                && last_read.elapsed() >= Duration::from_secs(1)
            {
                let thread_id = tracker
                    .lock()
                    .map_err(|_| "Session tracker lock poisoned")?
                    .current
                    .thread_id
                    .clone();
                if let Some(thread_id) = thread_id {
                    counter += 1;
                    let id = format!(
                        "prevx-metadata-{}-{connection_id}-{counter}",
                        std::process::id()
                    );
                    let request = json!({"id":id,"method":"thread/read","params":{"threadId":thread_id,"includeTurns":false}});
                    tracker
                        .lock()
                        .map_err(|_| "Session tracker lock poisoned")?
                        .request(connection_id, &request);
                    if let Err(error) = backend.send(Message::Text(request.to_string().into()))
                        && !blocked(&error)
                    {
                        return Err(error.into());
                    }
                    read_id = Some(id);
                }
                last_read = Instant::now();
            }
            if let Err(error) = websocket.flush()
                && !blocked(&error)
            {
                return Err(error.into());
            }
            if let Err(error) = backend.flush()
                && !blocked(&error)
            {
                return Err(error.into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })();
    let _ = backend.close(None);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_handshake_on_nonblocking_accepted_stream_succeeds() {
        let runtime = RuntimeDir::create().unwrap();
        let path = runtime.path().join("backend.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (client, accepted) = UnixStream::pair().unwrap();
        accepted.set_nonblocking(true).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let directory = runtime.path().to_owned();
        let worker = thread::spawn(move || {
            connection(
                accepted,
                path.to_str().unwrap(),
                &directory,
                &AtomicBool::new(true),
                &Mutex::new(Tracker::default()),
                1,
            )
            .map_err(|error| error.to_string())
        });
        thread::sleep(Duration::from_millis(100));
        let (frontend, _) = tungstenite::client("ws://localhost/rpc", client).unwrap();
        let (backend, _) = listener.accept().unwrap();
        let backend = tungstenite::accept(backend).unwrap();
        worker.join().unwrap().unwrap();
        drop((frontend, backend));
    }
}
