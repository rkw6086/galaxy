//! Shell completion generation for `galaxyctrl`.
use clap_complete::aot::{Shell, generate};
use local_control::protocol::{ControlError, ErrorCode};

use crate::local_control::{ControlArgs, normalized_control_command_name};

pub(super) fn generate_completions_to_stdout(shell: Option<Shell>) -> Result<(), ControlError> {
    let shell = shell.or_else(Shell::from_env).ok_or_else(|| {
        ControlError::new(
            ErrorCode::InvalidParams,
            "could not determine shell from environment; provide a shell argument",
        )
    })?;
    let mut cmd = ControlArgs::clap_command();
    let invocation_name = crate::binary_name();
    let bin_name = normalized_control_command_name(invocation_name.as_deref());
    generate(shell, &mut cmd, bin_name, &mut std::io::stdout());
    Ok(())
}

#[cfg(test)]
pub(crate) fn generate_completion_string(shell: Shell) -> Result<String, ControlError> {
    let mut cmd = ControlArgs::clap_command();
    let mut output = Vec::new();
    generate(shell, &mut cmd, "galaxyctrl", &mut output);
    String::from_utf8(output).map_err(|err| {
        ControlError::with_details(
            ErrorCode::Internal,
            "failed to render local-control completions",
            err.to_string(),
        )
    })
}
