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

fn convert_pdf(name: &str, pdf: &[u8], flags: &[&str]) -> (Output, Option<String>) {
    let input = tmp_path(&format!("{name}_in"));
    let output = tmp_path(&format!("{name}_out.md"));
    fs::write(&input, pdf).unwrap();

    let mut args = vec![
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        "--format",
        "pdf",
    ];
    args.extend(flags);

    let result = run(&args);
    let markdown = fs::read_to_string(&output).ok();

    fs::remove_file(&input).ok();
    fs::remove_file(&output).ok();

    (result, markdown)
}

fn assert_needs_ocr(result: &Output, markdown: Option<String>) {
    let stderr = String::from_utf8_lossy(&result.stderr);

    assert_eq!(result.status.code(), Some(3), "stderr: {stderr}");
    assert_eq!(stderr.trim_end(), "convert error: NeedsOcr");
    assert_eq!(
        markdown, None,
        "a PDF that needs OCR must write no markdown"
    );
}

/// One page of a test PDF: what its content stream draws.
#[derive(Clone, Copy)]
enum Page {
    /// Twenty-five lines of text.
    Text,
    /// A full-page image and nothing else: a scanned page.
    Scan,
    /// A full-page image under this many lines of text: a scan with a stamp,
    /// a header or a page number printed over it.
    StampedScan(usize),
    /// Twenty-five lines of text under a small image: a letterhead.
    TextWithLogo,
    /// Twenty-five lines of text in a font whose character map is broken,
    /// so the letters extract as other letters.
    GarbledText,
    /// Twenty-five lines of text whose spaces extract as `$`, what another
    /// kind of broken character map does.
    DollarsForSpaces,
    Blank,
}

const SCAN: &str = "q 612 0 0 792 0 0 cm /Im0 Do Q\n";
const LOGO: &str = "q 120 0 0 40 72 740 cm /Im0 Do Q\n";

fn text_lines(count: usize) -> String {
    lines_in_font(count, "F1")
}

fn lines_in_font(count: usize, font: &str) -> String {
    (0..count)
        .map(|line| {
            let y = 700 - 14 * line;
            format!("BT /{font} 10 Tf 72 {y} Td (Holding {line} of Avery Abernathy is worth 12,345.67 dollars) Tj ET\n")
        })
        .collect()
}

/// A ToUnicode map that is off by a constant for the lowercase letters, as a
/// broken font subset is: `a` extracts as `R`, `t` as `e`. The text is all
/// there and none of it is right.
fn broken_character_map() -> String {
    let cmap = "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
                /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
                1 begincodespacerange\n<00> <FF>\nendcodespacerange\n\
                1 beginbfrange\n<61> <7A> <0052>\nendbfrange\nendcmap\n\
                CMapName currentdict /CMap defineresource pop\nend\nend";

    format!("<< /Length {} >>\nstream\n{cmap}\nendstream", cmap.len())
}

/// A PDF of the given pages, each with only the resources it draws: a text
/// page that listed an image it never paints would read as an image page.
fn pdf(pages: &[Page]) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        String::new(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!(
            "<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray \
             /BitsPerComponent 8 /Length 64 >>\nstream\n{}\nendstream",
            "x".repeat(64)
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 6 0 R >>".to_string(),
        broken_character_map(),
    ];
    let mut kids = Vec::new();

    for page in pages {
        let (content, xobjects) = match page {
            Page::Text => (text_lines(25), ""),
            Page::Scan => (SCAN.to_string(), "/Im0 4 0 R"),
            Page::StampedScan(lines) => (format!("{SCAN}{}", text_lines(*lines)), "/Im0 4 0 R"),
            Page::TextWithLogo => (format!("{LOGO}{}", text_lines(25)), "/Im0 4 0 R"),
            Page::GarbledText => (lines_in_font(25, "F2"), ""),
            Page::DollarsForSpaces => (
                text_lines(25)
                    .replace("Holding ", "Holding$")
                    .replace(
                        " of Avery Abernathy is worth ",
                        "$of$Avery$Abernathy$is$worth$",
                    )
                    .replace(" dollars", "$dollars"),
                "",
            ),
            Page::Blank => (String::new(), ""),
        };

        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R /F2 5 0 R >> \
             /XObject << {xobjects} >> >> /Contents {} 0 R >>",
            objects.len()
        ));
        kids.push(format!("{} 0 R", objects.len()));
    }

    objects[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        kids.len()
    );

    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();

    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend(format!("{} 0 obj\n{object}\nendobj\n", index + 1).bytes());
    }

    let xref = bytes.len();
    bytes.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());

    for offset in offsets {
        bytes.extend(format!("{offset:010} 00000 n \n").bytes());
    }

    bytes.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .bytes(),
    );

    bytes
}

#[test]
fn text_pdf_converts_with_a_logo_and_a_blank_page() {
    let (result, markdown) = convert_pdf(
        "text",
        &pdf(&[Page::TextWithLogo, Page::Text, Page::Blank]),
        &[],
    );

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
    assert!(markdown.unwrap().contains("Avery Abernathy"));
}

#[test]
fn scanned_pdf_needs_ocr() {
    let (result, markdown) = convert_pdf("scan", &pdf(&[Page::Scan, Page::Scan]), &[]);

    assert_needs_ocr(&result, markdown);
}

#[test]
fn one_scanned_page_among_text_pages_needs_ocr() {
    // anydoc on its own converts this one: it samples eight pages and calls a
    // document text once 60% of them are, so the scanned page goes missing
    // from markdown that reads as complete. The scan is page 13, one of the
    // eight it samples here, and seven text pages of eight still pass.
    let mut pages = vec![Page::Text; 30];
    pages[12] = Page::Scan;

    let (result, markdown) = convert_pdf("mixed", &pdf(&pages), &[]);

    assert_needs_ocr(&result, markdown);
}

#[test]
fn scan_under_a_few_lines_of_text_needs_ocr() {
    // A scanned form with an envelope id, a footer and a page number printed
    // over it has a text layer, and none of the form is in it.
    let (result, markdown) = convert_pdf("stamped", &pdf(&[Page::Text, Page::StampedScan(3)]), &[]);

    assert_needs_ocr(&result, markdown);
}

#[test]
fn garbled_text_among_good_pages_exits_7() {
    // anydoc on its own converts the second of these, with a warning in its
    // log: pdf-inspector flags the document and names no page.
    for (name, page) in [
        ("garbled_letters", Page::GarbledText),
        ("garbled_spaces", Page::DollarsForSpaces),
    ] {
        let (result, markdown) = convert_pdf(name, &pdf(&[Page::Text, page, Page::Text]), &[]);

        assert_eq!(result.status.code(), Some(7), "{name}");
        assert_eq!(
            String::from_utf8_lossy(&result.stderr).trim(),
            "convert error: GarbledText"
        );
        assert!(markdown.is_none(), "garbled text must not be written");
    }
}

#[test]
fn skipping_scanned_pages_converts_the_pages_that_have_text_and_names_the_rest() {
    let unread = tmp_path("skip_unread");

    let (result, markdown) = convert_pdf(
        "skip",
        &pdf(&[Page::Text, Page::Scan, Page::Text, Page::StampedScan(3)]),
        &[
            "--scanned-pages",
            "skip",
            "--unread-pages-out",
            unread.to_str().unwrap(),
        ],
    );

    assert_eq!(
        result.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(markdown.unwrap().contains("Avery Abernathy"));
    assert_eq!(fs::read_to_string(&unread).unwrap(), "2,4");

    fs::remove_file(&unread).ok();
}

#[test]
fn a_whole_conversion_names_no_unread_pages() {
    let unread = tmp_path("whole_unread");

    let (result, _markdown) = convert_pdf(
        "whole",
        &pdf(&[Page::Text, Page::Blank]),
        &["--unread-pages-out", unread.to_str().unwrap()],
    );

    assert_eq!(result.status.code(), Some(0));
    assert_eq!(fs::read_to_string(&unread).unwrap(), "");

    fs::remove_file(&unread).ok();
}

#[test]
fn skipping_scanned_pages_still_fails_a_pdf_with_no_text() {
    let (result, markdown) = convert_pdf(
        "skip_all",
        &pdf(&[Page::Scan, Page::Scan]),
        &["--scanned-pages", "skip"],
    );

    assert_needs_ocr(&result, markdown);
}

#[test]
fn password_protected_pdf_exits_5() {
    // qpdf --encrypt --user-password=secret --owner-password=owner --bits=256
    let (result, markdown) = convert_pdf(
        "encrypted",
        include_bytes!("fixtures/password_protected.pdf"),
        &[],
    );

    assert_eq!(result.status.code(), Some(5));
    assert_eq!(
        String::from_utf8_lossy(&result.stderr).trim_end(),
        "convert error: Encrypted"
    );
    assert_eq!(markdown, None);
}

#[cfg(unix)]
#[test]
fn conversion_past_its_timeout_exits_4() {
    // Reading a FIFO nobody writes to never returns, as a parser stuck on a
    // pathological file never would.
    let input = tmp_path("fifo_in");
    let output = tmp_path("fifo_out.md");
    let made = Command::new("mkfifo").arg(&input).status().unwrap();
    assert!(made.success());

    let result = run(&[
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        "--format",
        "pdf",
        "--timeout-secs",
        "1",
    ]);

    assert_eq!(result.status.code(), Some(4));
    assert_eq!(
        String::from_utf8_lossy(&result.stderr).trim_end(),
        "timed out after 1s"
    );
    assert!(!output.exists());

    fs::remove_file(&input).ok();
}

#[test]
fn bad_flags_exit_1() {
    for [flag, value] in [
        ["--timeout-secs", "0"],
        ["--timeout-secs", "soon"],
        ["--scanned-pages", "some"],
        ["--deadline", "5"],
    ] {
        let result = run(&["in", "out", flag, value]);

        assert_eq!(result.status.code(), Some(1), "flag: {flag} {value}");
        assert!(String::from_utf8_lossy(&result.stderr).contains("Usage:"));
    }
}
