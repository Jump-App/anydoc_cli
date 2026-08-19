use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_anydoc_cli"))
        .args(args)
        .output()
        .expect("failed to run anydoc_cli")
}

fn tmp_path(name: &str) -> PathBuf {
    env::temp_dir().join(format!("anydoc_cli_test_{}_{name}", std::process::id()))
}

#[test]
fn converts_csv_to_markdown_table() {
    let input = tmp_path("csv_in");
    let output = tmp_path("csv_out.md");
    fs::write(&input, "name,balance\nAcme,100.50\n").unwrap();

    let result = run(&[
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        "--format",
        "csv",
    ]);

    assert_eq!(
        result.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stdout.is_empty(),
        "stdout must never carry document content"
    );
    let markdown = fs::read_to_string(&output).unwrap();
    assert!(markdown.contains("Acme"));
    assert!(markdown.contains("100.50"));
    assert!(markdown.contains('|'), "expected a markdown table");

    fs::remove_file(&input).ok();
    fs::remove_file(&output).ok();
}

#[test]
fn csv_without_format_hint_is_unsupported() {
    // CSV carries no content signature and the caller passes extension-less
    // paths, so sniffing must fail rather than guess.
    let input = tmp_path("sniff_in");
    let output = tmp_path("sniff_out.md");
    fs::write(&input, "name,balance\nAcme,100.50\n").unwrap();

    let result = run(&[input.to_str().unwrap(), output.to_str().unwrap()]);

    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("convert error: Unsupported"),
        "stderr: {stderr}"
    );

    fs::remove_file(&input).ok();
}

#[test]
fn malformed_input_exits_2_with_variant_name_only() {
    let input = tmp_path("garbage_in");
    let output = tmp_path("garbage_out.md");
    let garbage: Vec<u8> = (0..512u32).map(|i| (i * 31 % 251) as u8).collect();
    fs::write(&input, garbage).unwrap();

    let result = run(&[
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        "--format",
        "pdf",
    ]);

    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.starts_with("convert error: "), "stderr: {stderr}");
    // Bounded output: the variant name, never document content or details.
    assert!(
        stderr.trim_end().lines().count() == 1 && stderr.len() < 64,
        "stderr: {stderr}"
    );

    fs::remove_file(&input).ok();
}

#[test]
fn unrecognized_format_exits_1() {
    let result = run(&["in", "out", "--format", "nope"]);

    assert_eq!(result.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("unrecognized format"), "stderr: {stderr}");
}

#[test]
fn wrong_arg_count_exits_1_with_usage() {
    let result = run(&["only-one-arg"]);

    assert_eq!(result.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("Usage:"), "stderr: {stderr}");
}

#[test]
fn missing_input_file_exits_1() {
    let output = tmp_path("missing_out.md");

    let result = run(&[
        "/nonexistent/anydoc_cli_test_input",
        output.to_str().unwrap(),
        "--format",
        "pdf",
    ]);

    assert_eq!(result.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("failed to read input"), "stderr: {stderr}");
}
