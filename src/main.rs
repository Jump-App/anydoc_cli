//! Convert a document to GitHub-Flavored Markdown using the `anydoc` crate.
//!
//! Usage: `anydoc_cli <input-path> <output-path> [--format <fmt>]`
//!
//! The markdown is written to `<output-path>`. Document content must never
//! reach stdout or stderr: the caller (an Elixir port) logs everything the
//! process prints, and those logs are exported. Errors print only bounded,
//! content-free messages.
//!
//! Exit codes: 0 = success, 1 = usage or I/O error, 2 = conversion error
//! (the `ConvertError` variant name is printed to stderr).

use std::env;
use std::fs;
use std::process;

use anydoc::{ConvertError, Format};

fn main() {
    let args: Vec<String> = env::args().collect();

    let (input_path, output_path, format) = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("Usage: anydoc_cli <input-path> <output-path> [--format <fmt>]");
            process::exit(1);
        }
    };

    let bytes = fs::read(input_path).unwrap_or_else(|e| {
        eprintln!("failed to read input: {}", e.kind());
        process::exit(1);
    });

    let markdown = anydoc::to_markdown_bytes(&bytes, format).unwrap_or_else(|e| {
        eprintln!("convert error: {}", variant_name(&e));
        process::exit(2);
    });

    fs::write(output_path, markdown).unwrap_or_else(|e| {
        eprintln!("failed to write output: {}", e.kind());
        process::exit(1);
    });
}

/// `--format` takes the bare extension anydoc recognizes (`pdf`, `docx`,
/// `xlsx`, ...). When omitted, the format is detected from the content,
/// which signature-less formats (CSV) cannot rely on.
fn parse_args(args: &[String]) -> Result<(&str, &str, Option<Format>), String> {
    match args {
        [_, input, output] => Ok((input, output, None)),
        [_, input, output, flag, fmt] if flag == "--format" => match Format::from_extension(fmt) {
            Some(format) => Ok((input, output, Some(format))),
            None => Err(format!("unrecognized format: {fmt}")),
        },
        _ => Err("expected <input-path> <output-path> [--format <fmt>]".to_string()),
    }
}

/// Only the variant name: `ConvertError`'s Display messages carry details
/// that may quote parts of the document.
fn variant_name(error: &ConvertError) -> &'static str {
    match error {
        ConvertError::Unsupported(_) => "Unsupported",
        ConvertError::Malformed { .. } => "Malformed",
        ConvertError::Encrypted => "Encrypted",
        ConvertError::ResourceLimit { .. } => "ResourceLimit",
        ConvertError::MissingPart { .. } => "MissingPart",
        ConvertError::Io(_) => "Io",
        _ => "Unknown",
    }
}
