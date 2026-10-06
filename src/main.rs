use std::process::ExitCode;

fn main() -> ExitCode {
    match codex_panel::app::run(std::env::args_os().skip(1).collect()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("codex-panel: {error}");
            ExitCode::FAILURE
        }
    }
}
