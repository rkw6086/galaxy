//! Generates a JSON Schema file describing Galaxy's user-facing settings.
//!
//! Usage:
//! ```
//! cargo run --bin generate_settings_schema -- [--channel dev|preview|stable] [output_path]
//! ```

use std::io::Write;

use galaxy::settings::schema_export::generate_settings_schema;

/// Ensures all `inventory::submit!` registrations from the app crate's
/// dependency tree are linked into the binary.
fn ensure_settings_linked() {
    let _ = std::hint::black_box(galaxy::settings::RESTORE_SESSION);
}

fn main() {
    ensure_settings_linked();

    let args: Vec<String> = std::env::args().collect();
    let mut channel = "dev";
    let mut output_path: Option<&str> = None;
    let mut index = 1;

    while index < args.len() {
        match args[index].as_str() {
            "--channel" => {
                index += 1;
                if index < args.len() {
                    channel = &args[index];
                }
            }
            arg if !arg.starts_with('-') => {
                output_path = Some(arg);
            }
            other => {
                eprintln!("Unknown argument: {other}");
                std::process::exit(1);
            }
        }
        index += 1;
    }

    let (output, entry_count) = generate_settings_schema(channel);

    if let Some(path) = output_path {
        let mut file = std::fs::File::create(path)
            .unwrap_or_else(|error| panic!("Failed to create output file '{path}': {error}"));
        file.write_all(output.as_bytes())
            .unwrap_or_else(|error| panic!("Failed to write to '{path}': {error}"));
        eprintln!("Wrote {entry_count} settings to {path}");
    } else {
        println!("{output}");
    }
}
