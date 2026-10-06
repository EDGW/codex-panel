//! Runtime state files and ownership of the temporary directory.
use crate::{AppResult, session::Session};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct RuntimeDir {
    path: PathBuf,
    cleanup: bool,
}

impl RuntimeDir {
    pub fn create() -> AppResult<Self> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = PathBuf::from(format!("/tmp/cc-panel-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path)?;
        let runtime = Self {
            path,
            cleanup: true,
        };
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700))?;
        runtime.save_session(&Session::default())?;
        Ok(runtime)
    }

    /// Child processes access state but do not own directory cleanup.
    pub fn open(path: PathBuf) -> Self {
        Self {
            path,
            cleanup: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn proxy_socket(&self) -> PathBuf {
        self.path.join("rpc.sock")
    }

    pub fn session(&self) -> Session {
        std::fs::read(self.path.join("session.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save_session(&self, session: &Session) -> AppResult<()> {
        let temporary = self.path.join("session.json.tmp");
        std::fs::write(&temporary, serde_json::to_vec(session)?)?;
        std::fs::rename(temporary, self.path.join("session.json"))?;
        Ok(())
    }

    pub fn save_bridge_error(&self, error: &str) {
        let _ = std::fs::write(self.path.join("bridge-error.txt"), error);
    }

    pub fn save_exit(&self, output: &str) -> AppResult<()> {
        std::fs::write(self.path.join("exit.txt"), output)?;
        Ok(())
    }

    pub fn exit_output(&self) -> Option<String> {
        std::fs::read_to_string(self.path.join("exit.txt")).ok()
    }

    /// Hand directory ownership to the tmux session when leaving it running.
    pub fn preserve(mut self) {
        self.cleanup = false;
    }
}

impl Drop for RuntimeDir {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_owner_cleans_up_but_child_and_detached_handles_preserve_state() {
        let runtime = RuntimeDir::create().unwrap();
        let path = runtime.path().to_owned();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(runtime.session(), Session::default());
        let child = RuntimeDir::open(path.clone());
        child.save_exit("\x1b[36mexit summary\x1b[0m").unwrap();
        drop(child);
        assert!(path.exists());
        assert_eq!(
            runtime.exit_output().unwrap(),
            "\x1b[36mexit summary\x1b[0m"
        );
        runtime.preserve();
        assert!(path.exists());
        // Take ownership back to exercise ordinary cleanup.
        drop(RuntimeDir {
            path: path.clone(),
            cleanup: true,
        });
        assert!(!path.exists());
    }

    #[test]
    fn invalid_or_missing_session_state_is_tolerated_during_startup() {
        let runtime = RuntimeDir::create().unwrap();
        std::fs::write(runtime.path.join("session.json"), "partial JSON").unwrap();
        assert_eq!(runtime.session(), Session::default());
    }
}
