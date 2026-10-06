//! All tmux commands, terminal sizing and shell-command encoding live here.
use crate::{AppResult, subprocess};
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Output};

const SESSION: &str = "panel";
const CODEX_PANE: &str = "panel:0.0";
pub const PANEL_HEIGHT: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneSize {
    pub height: usize,
    pub width: usize,
}

pub struct Tmux {
    socket: String,
}

impl Tmux {
    pub fn new(socket: String) -> Self {
        Self { socket }
    }

    pub fn socket(&self) -> &str {
        &self.socket
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("tmux");
        command
            .args(["-L", &self.socket, "-f", "/dev/null"])
            .args(args);
        command
    }

    fn execute(&self, args: &[&str]) -> AppResult<Output> {
        let operation = args.first().copied().unwrap_or("command");
        let output = subprocess::output(
            &mut self.command(args),
            &format!("Could not run tmux `{operation}`"),
        )?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr = stderr.trim();
            let detail = if stderr.is_empty() {
                output.status.to_string()
            } else {
                format!("{}: {stderr}", output.status)
            };
            return Err(format!("tmux `{operation}` failed ({detail})").into());
        }
        Ok(output)
    }

    /// Gate the upper process until the layout is ready, then close this tmux session on exit.
    pub fn managed_command(&self, child_command: &str) -> String {
        format!(
            "tmux -L {} wait-for ready; {}; result=$?; tmux -L {} kill-session -t {}; exit $result",
            quote(&self.socket),
            child_command,
            quote(&self.socket),
            quote(SESSION)
        )
    }

    pub fn create_layout(
        &self,
        cwd: &Path,
        codex_command: &str,
        panel_command: &str,
    ) -> AppResult<()> {
        let cwd = path_argument(cwd)?;
        self.execute(&[
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-x",
            "100",
            "-y",
            "30",
            "-c",
            &cwd,
            codex_command,
        ])?;
        // Keep the upper pane long enough for pane-died to run even if its
        // supervisor/shell is killed before managed_command can clean up.
        let pane = self.execute(&["display-message", "-p", "-t", CODEX_PANE, "#{pane_id}"])?;
        let pane = String::from_utf8(pane.stdout)?;
        self.execute(&["set-option", "-p", "-t", CODEX_PANE, "remain-on-exit", "on"])?;
        self.execute(&[
            "set-hook",
            "-t",
            SESSION,
            "pane-died",
            &format!(
                "if-shell -F {} {}",
                // pane_id follows the active pane; hook_pane identifies the pane that died.
                quote(&format!("#{{==:#{{hook_pane}},{}}}", pane.trim())),
                quote("kill-session -t panel")
            ),
        ])?;
        self.execute(&["set-option", "-t", SESSION, "status", "off"])?;
        // Capture wheel events so terminals do not translate scrolling into arrow keys.
        self.execute(&["set-option", "-t", SESSION, "mouse", "on"])?;
        self.execute(&[
            "split-window",
            "-v",
            "-l",
            &PANEL_HEIGHT.to_string(),
            "-t",
            CODEX_PANE,
            "-c",
            &cwd,
            panel_command,
        ])?;
        self.execute(&["select-pane", "-t", CODEX_PANE])?;
        self.execute(&["wait-for", "-S", "ready"])?;
        Ok(())
    }

    pub fn pane_size(&self, pane: &str) -> AppResult<PaneSize> {
        let output = self.execute(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_height} #{pane_width}",
        ])?;
        parse_size(&String::from_utf8(output.stdout)?)
    }

    pub fn resize_panel(&self, pane: &str, height: usize) -> AppResult<()> {
        self.execute(&["resize-pane", "-t", pane, "-y", &height.to_string()])?;
        Ok(())
    }

    pub fn pane_active(&self, pane: &str) -> AppResult<bool> {
        let output = self.execute(&["display-message", "-p", "-t", pane, "#{pane_active}"])?;
        Ok(output.stdout == b"1\n")
    }

    pub fn focus(&self, settings: bool, panel: &str) -> AppResult<()> {
        let target = if settings { panel } else { CODEX_PANE };
        let active = self.execute(&["display-message", "-p", "-t", target, "#{pane_active}"])?;
        if active.stdout != b"1\n" {
            self.execute(&["select-pane", "-t", target])?;
        }
        Ok(())
    }

    pub fn capture(&self, pane: &str) -> AppResult<String> {
        // Preserve ANSI colors and join soft wraps in the native exit summary.
        let output = self.execute(&["capture-pane", "-e", "-J", "-p", "-t", pane])?;
        Ok(String::from_utf8(output.stdout)?)
    }

    pub fn attach(&self) -> AppResult<()> {
        let status = subprocess::status(
            self.command(&["attach-session", "-t", SESSION])
                .env_remove("TMUX"),
            "Could not run tmux `attach-session`",
        )?;
        if !status.success() {
            return Err(format!("tmux `attach-session` failed ({status})").into());
        }
        Ok(())
    }

    pub fn stop(&self) {
        let _ = self.execute(&["kill-server"]);
    }

    pub fn session_exists(&self) -> AppResult<bool> {
        Ok(subprocess::output(
            &mut self.command(&["has-session", "-t", SESSION]),
            "Could not run tmux `has-session`",
        )?
        .status
        .success())
    }
}

/// Encode a child process for tmux's shell while keeping role metadata out of its argv.
pub(crate) fn child_command(
    executable: &Path,
    args: &[OsString],
    environment: &[(&str, OsString)],
) -> AppResult<String> {
    let mut command = vec!["env".to_owned()];
    for (name, value) in environment {
        command.push(format!("{name}={}", text(value)?));
    }
    command.push(path_argument(executable)?);
    for arg in args {
        command.push(text(arg)?.to_owned());
    }
    Ok(shell_command(&command))
}

fn text(value: &OsStr) -> AppResult<&str> {
    value
        .to_str()
        .ok_or_else(|| "tmux command is not valid UTF-8".into())
}

fn path_argument(path: &Path) -> AppResult<String> {
    Ok(text(path.as_os_str())?.to_owned())
}

fn parse_size(text: &str) -> AppResult<PaneSize> {
    let dimensions: Vec<usize> = text
        .split_whitespace()
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let [height, width] = dimensions.as_slice() else {
        return Err("Invalid pane dimensions".into());
    };
    Ok(PaneSize {
        height: *height,
        width: *width,
    })
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn shell_command(args: &[String]) -> String {
    args.iter()
        .map(|value| quote(value))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_arguments_round_trip_without_interpolation_or_splitting() {
        let values = [
            "a b",
            "'single'",
            "$(printf hacked)",
            "`printf hacked`",
            "$HOME",
            "",
            "line\nbreak",
        ];
        let mut command = vec!["printf".into(), "%s\\0".into()];
        command.extend(values.map(str::to_owned));
        let output = Command::new("/bin/sh")
            .args(["-c", &shell_command(&command)])
            .output()
            .unwrap();
        assert!(output.status.success());
        let expected = values.join("\0") + "\0";
        assert_eq!(output.stdout, expected.as_bytes());
    }

    #[test]
    fn wrapped_child_keeps_environment_arguments_and_original_exit_status() {
        let tmux = Tmux::new("test socket".into());
        let args = vec![
            OsString::from("-c"),
            OsString::from("printf '%s\\0' \"$CC_PANEL_TEST\" \"$@\"; exit 7"),
            OsString::from("sh"),
            OsString::from("argument with spaces"),
            OsString::from("$(printf hacked)"),
        ];
        let child = child_command(
            Path::new("/bin/sh"),
            &args,
            &[("CC_PANEL_TEST", OsString::from("literal $(printf hacked)"))],
        )
        .unwrap();
        let output = Command::new("/bin/sh")
            .args([
                "-c",
                &format!("tmux() {{ :; }}\n{}", tmux.managed_command(&child)),
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(
            output.stdout,
            b"literal $(printf hacked)\0argument with spaces\0$(printf hacked)\0"
        );
    }
}
