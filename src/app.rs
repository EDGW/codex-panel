//! Application composition and process orchestration. Public argv always belongs to Codex.
use crate::{
    AppResult, codex, config, panel,
    runtime::RuntimeDir,
    tmux::{self, Tmux},
};
use std::env;
use std::ffi::OsString;
use std::io::{self, IsTerminal};
use std::process::ExitCode;

pub fn run(args: Vec<OsString>) -> AppResult<ExitCode> {
    match env::var(codex::PROCESS_KIND).ok().as_deref() {
        Some("codex") => {
            let runtime = RuntimeDir::open(required_path(codex::RUNTIME_DIR)?);
            let tmux = Tmux::new(env::var(codex::TMUX_SOCKET)?);
            let result = codex::run(&runtime, &args, &tmux, &env::var("TMUX_PANE")?);
            if let Err(error) = &result {
                // The shell closes tmux on failure too; replay the diagnostic
                // after returning to the caller's terminal.
                runtime.save_exit(&format!("codex-panel: {error}"))?;
            }
            let status = result?;
            Ok(exit_code(status))
        }
        Some("panel") => {
            let runtime = RuntimeDir::open(required_path(codex::RUNTIME_DIR)?);
            let tmux = Tmux::new(env::var(codex::TMUX_SOCKET)?);
            let config = config::load(&config::discover()?)?;
            panel::run(&tmux, &runtime, &env::var("TMUX_PANE")?, config)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(_) => Err("Invalid internal panel process kind".into()),
        None => {
            // Preserve native pipes, help output and exec mode outside an interactive terminal.
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                return Ok(exit_code(codex::passthrough(&args)?));
            }
            launch(&args)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn exit_code(status: std::process::ExitStatus) -> ExitCode {
    ExitCode::from(status.code().unwrap_or(1) as u8)
}

fn required_path(name: &str) -> AppResult<std::path::PathBuf> {
    Ok(env::var_os(name)
        .ok_or_else(|| format!("Missing internal environment variable {name}"))?
        .into())
}

fn launch(args: &[OsString]) -> AppResult<()> {
    let paths = config::discover()?;
    config::load(&paths)?;
    let runtime = RuntimeDir::create()?;
    let tmux = Tmux::new(format!("codex-panel-{}", std::process::id()));
    let executable = env::current_exe()?;
    let secondary_gray = panel::theme::detect_secondary()
        .map(|gray| gray.to_string())
        .unwrap_or_default();
    let child_environment = |kind: &str| {
        let mut environment = vec![
            (codex::PROCESS_KIND, OsString::from(kind)),
            (codex::RUNTIME_DIR, runtime.path().as_os_str().to_owned()),
            (codex::TMUX_SOCKET, OsString::from(tmux.socket())),
            (
                "CC_PANEL_DEFAULTS_CONFIG",
                paths.defaults.as_os_str().to_owned(),
            ),
        ];
        if let Some(user) = &paths.user {
            environment.push(("CC_PANEL_CONFIG", user.as_os_str().to_owned()));
        }
        if kind == "panel" {
            environment.push((
                panel::theme::SECONDARY_GRAY,
                OsString::from(&secondary_gray),
            ));
        }
        environment
    };
    let codex = tmux.managed_command(&tmux::child_command(
        &executable,
        args,
        &child_environment("codex"),
    )?);
    let panel = tmux::child_command(&executable, &[], &child_environment("panel"))?;
    if let Err(error) = tmux.create_layout(&env::current_dir()?, &codex, &panel) {
        tmux.stop();
        return Err(error);
    }
    if let Err(error) = tmux.attach() {
        tmux.stop();
        return Err(error);
    }
    if let Some(output) = runtime.exit_output() {
        if !output.trim().is_empty() {
            println!("{}\x1b[0m", output.trim());
        }
    } else if tmux.session_exists()? {
        // Ctrl-b d detaches; the live child processes still need their state and proxy socket.
        runtime.preserve();
    }
    Ok(())
}
