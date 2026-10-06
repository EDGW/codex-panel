//! Add executable and operation context to process-launch errors.
use crate::AppResult;
use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus, Output};

pub(crate) fn output(command: &mut Command, context: &str) -> AppResult<Output> {
    let program = command.get_program().to_owned();
    command
        .output()
        .map_err(|error| launch_error(&program, context, error))
}

pub(crate) fn status(command: &mut Command, context: &str) -> AppResult<ExitStatus> {
    let program = command.get_program().to_owned();
    command
        .status()
        .map_err(|error| launch_error(&program, context, error))
}

fn launch_error(
    program: &std::ffi::OsStr,
    context: &str,
    error: io::Error,
) -> Box<dyn std::error::Error> {
    let program = program.to_string_lossy();
    let detail = match error.kind() {
        io::ErrorKind::NotFound if Path::new(program.as_ref()).components().count() == 1 => {
            format!(
                "command `{program}` was not found; install it and ensure it is available in PATH"
            )
        }
        io::ErrorKind::NotFound => {
            format!("executable `{program}` was not found; check that it and its interpreter exist")
        }
        io::ErrorKind::PermissionDenied => {
            format!("cannot execute `{program}`: permission denied")
        }
        _ => format!("cannot execute `{program}`: {error}"),
    };
    format!("{context}: {detail}").into()
}
