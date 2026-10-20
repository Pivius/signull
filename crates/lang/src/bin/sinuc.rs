//! `sinuc` — the sinu compiler.
//!
//! Compiles a `.sn` source file into a serialized `ProgramImage` the kernel
//! `exec` path can load.
//!
//! Usage: `sinuc <file.sn> [-o <out.snx>]`

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-o" | "--output" => {
                let Some(path) = it.next() else {
                    eprintln!("sinuc: `-o` requires a path");
                    return ExitCode::from(2);
                };
                output = Some(PathBuf::from(path));
            }
            "-h" | "--help" => {
                println!("usage: sinuc <file.sn> [-o <out.snx>]");
                return ExitCode::SUCCESS;
            }
            other if !other.starts_with('-') && input.is_none() => {
                input = Some(PathBuf::from(other));
            }
            other => {
                eprintln!("sinuc: unknown argument `{other}`");
                return ExitCode::from(2);
            }
        }
    }

    let Some(input) = input else {
        eprintln!("sinuc: no input file\nusage: sinuc <file.sn> [-o <out.snx>]");
        return ExitCode::from(2);
    };
    let output = output.unwrap_or_else(|| input.with_extension("snx"));

    let source = match std::fs::read_to_string(&input) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("sinuc: cannot read {}: {err}", input.display());
            return ExitCode::FAILURE;
        }
    };

    let image = match lang::compile(&source) {
        Ok(image) => image,
        Err(err) => {
            eprintln!("{}: {err}", input.display());
            return ExitCode::FAILURE;
        }
    };

    let bytes = lang::encode_image(&image);
    if let Err(err) = write_image(&output, &bytes) {
        eprintln!("sinuc: cannot write {}: {err}", output.display());
        return ExitCode::FAILURE;
    }

    println!(
        "{}: compiled {} instructions, {} data blob(s) -> {}",
        input.display(),
        image.code.len(),
        image.data.len(),
        output.display()
    );
    ExitCode::SUCCESS
}

fn write_image(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}
