//! Preserve Cargo's response files on Windows while caching ordinary compilations.
//!
//! sccache 0.19.0 expands @argfiles before spawning rustc, reintroducing the
//! Windows command-line limit that Cargo avoided. Forward those invocations to
//! rustc unchanged; no response-file contents or compiler arguments are logged.

use std::env;
use std::ffi::OsString;
use std::process::{self, Command};

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let mut invocation = env::args_os().skip(1);
    let compiler = invocation.next().ok_or("Missing rustc executable")?;
    let arguments: Vec<OsString> = invocation.collect();
    let uses_response_file = arguments
        .iter()
        .any(|argument| argument.as_encoded_bytes().starts_with(b"@"));
    let mut command = if uses_response_file {
        eprintln!("Galaxy compiler cache: preserving rustc response file; this invocation runs without sccache.");
        Command::new(compiler)
    } else {
        let cache =
            env::var_os("GALAXY_SCCACHE_EXECUTABLE").ok_or("Missing compiler cache executable")?;
        let mut command = Command::new(cache);
        command.arg(compiler);
        command
    };
    Ok(command.args(arguments).status()?.code().unwrap_or(1))
}

fn main() {
    process::exit(match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Galaxy compiler cache wrapper failed: {error}");
            1
        }
    });
}
