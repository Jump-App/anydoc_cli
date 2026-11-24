//! Convert a document to GitHub-Flavored Markdown using the `anydoc` crate.
//!
//! Usage: `anydoc_cli <input-path> <output-path> [--format <fmt>] [--timeout-secs <n>]
//! [--scanned-pages <fail|skip>] [--unread-pages-out <path>]`
//!
//! The markdown is written to `<output-path>`. Document content must never
//! reach stdout or stderr: the caller (an Elixir port) logs everything the
//! process prints, and those logs are exported. Errors print only bounded,
//! content-free messages.
//!
//! The caller cannot read what this process prints, so the exit code is the
//! whole contract: every outcome the caller treats differently has its own.
//!
//! Exit codes: 0 = success, 1 = usage or I/O error, 2 = conversion error
//! (the `ConvertError` variant name is printed to stderr), 3 = needs OCR,
//! 4 = timed out, 5 = encrypted, 6 = resource limit, 7 = garbled text.
//!
//! Success means the markdown holds every page, unless the caller passed
//! `--scanned-pages skip`. A PDF with a page this process cannot vouch for
//! exits 3 or 7 and writes nothing, because markdown that is missing a page
//! or holds the wrong characters reads as complete (see `pages_needing_ocr`
//! and `complete_pdf_to_markdown`).
//!
//! A PDF goes to pdf-inspector directly, the library anydoc converts PDFs
//! with: which pages are scanned and whether the text came out garbled are
//! answers anydoc does not pass on.

use std::env;
use std::fs;
use std::process;
use std::thread;
use std::time::Duration;

use anydoc::{ConvertError, Format};
use pdf_inspector::{DetectionConfig, PdfError, ScanStrategy};

const EXIT_USAGE_OR_IO: i32 = 1;
const EXIT_CONVERT_ERROR: i32 = 2;
const EXIT_NEEDS_OCR: i32 = 3;
const EXIT_TIMED_OUT: i32 = 4;
const EXIT_ENCRYPTED: i32 = 5;
const EXIT_RESOURCE_LIMIT: i32 = 6;
const EXIT_GARBLED_TEXT: i32 = 7;

const USAGE: &str = "Usage: anydoc_cli <input-path> <output-path> [--format <fmt>] \
                     [--timeout-secs <n>] [--scanned-pages <fail|skip>] \
                     [--unread-pages-out <path>]";

/// A page that carries an image is trusted to its text layer only from this
/// many text-showing operators up. Below it the text is a stamp, a header or
/// a page number over a scan, not the page's content.
const MIN_TEXT_OPS_ON_IMAGE_PAGE: u32 = 10;

/// pdf-inspector's reason for a page with no text and no image: a blank page.
/// Nothing on it for OCR to read, so it does not make a document need OCR.
const REASON_BLANK_PAGE: &str = "no_text";

/// What a PDF's scanned pages do to the conversion.
#[derive(Clone, Copy)]
enum ScannedPages {
    /// One page that needs OCR fails the document: the markdown is complete
    /// or there is none. For a caller that has an OCR parser to fall back to.
    Fail,
    /// The pages that have text are converted and the rest are left out.
    /// For a caller with nothing to fall back to, which would rather read
    /// most of a document than none of it. `--unread-pages-out` says which
    /// pages those were.
    Skip,
}

/// Why a document produced no markdown.
enum Failure {
    /// A PDF with a page whose content is in pixels.
    NeedsOcr,
    /// A PDF whose text layer is not the text on the page: its fonts do not
    /// say which characters they draw, so extraction returns other ones.
    GarbledText,
    Convert(ConvertError),
}

struct Converted {
    markdown: String,
    /// The pages of a PDF whose content, or part of it, is in pixels and so
    /// is not in the markdown. Empty unless scanned pages were skipped.
    unread_pages: Vec<u32>,
}

struct Args<'a> {
    input_path: &'a str,
    output_path: &'a str,
    format: Option<Format>,
    timeout: Option<Duration>,
    scanned_pages: ScannedPages,
    unread_pages_out: Option<&'a str>,
}

fn main() {
    let raw: Vec<String> = env::args().collect();

    let args = match parse_args(&raw) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{USAGE}");
            process::exit(EXIT_USAGE_OR_IO);
        }
    };

    if let Some(timeout) = args.timeout {
        arm_watchdog(timeout);
    }

    let bytes = fs::read(args.input_path).unwrap_or_else(|e| {
        eprintln!("failed to read input: {}", e.kind());
        process::exit(EXIT_USAGE_OR_IO);
    });

    let converted =
        to_markdown(&bytes, args.format, args.scanned_pages).unwrap_or_else(|failure| {
            eprintln!("convert error: {}", failure_name(&failure));
            process::exit(exit_code(&failure));
        });

    write(args.output_path, &converted.markdown);

    if let Some(path) = args.unread_pages_out {
        write(path, &page_list(&converted.unread_pages));
    }
}

fn write(path: &str, contents: &str) {
    fs::write(path, contents).unwrap_or_else(|e| {
        eprintln!("failed to write output: {}", e.kind());
        process::exit(EXIT_USAGE_OR_IO);
    });
}

/// Page numbers from 1, comma separated; empty when there are none.
fn page_list(pages: &[u32]) -> String {
    pages
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Two positional paths, then flags in any order.
///
/// `--format` takes the bare extension anydoc recognizes (`pdf`, `docx`,
/// `xlsx`, ...). When omitted, the format is detected from the content,
/// which signature-less formats (CSV) cannot rely on.
///
/// `--timeout-secs` bounds the whole run. When omitted, nothing does.
///
/// `--scanned-pages` is `fail` when omitted: leaving pages out is something
/// a caller asks for, never something it gets by forgetting a flag.
///
/// `--unread-pages-out` names a file that gets the numbers of the pages left
/// out of the markdown, written with it and empty when every page is in.
fn parse_args(args: &[String]) -> Result<Args<'_>, String> {
    let [_, input_path, output_path, flags @ ..] = args else {
        return Err("expected <input-path> <output-path>".to_string());
    };

    let mut parsed = Args {
        input_path,
        output_path,
        format: None,
        timeout: None,
        scanned_pages: ScannedPages::Fail,
        unread_pages_out: None,
    };

    for pair in flags.chunks(2) {
        match pair {
            [flag, value] if flag == "--format" => match Format::from_extension(value) {
                Some(format) => parsed.format = Some(format),
                None => return Err(format!("unrecognized format: {value}")),
            },
            [flag, value] if flag == "--timeout-secs" => match value.parse::<u64>() {
                Ok(seconds) if seconds > 0 => parsed.timeout = Some(Duration::from_secs(seconds)),
                _ => return Err(format!("invalid timeout: {value}")),
            },
            [flag, value] if flag == "--scanned-pages" => match value.as_str() {
                "fail" => parsed.scanned_pages = ScannedPages::Fail,
                "skip" => parsed.scanned_pages = ScannedPages::Skip,
                _ => return Err(format!("unrecognized scanned pages mode: {value}")),
            },
            [flag, value] if flag == "--unread-pages-out" => parsed.unread_pages_out = Some(value),
            [flag, _] => return Err(format!("unrecognized flag: {flag}")),
            _ => return Err("a flag is missing its value".to_string()),
        }
    }

    Ok(parsed)
}

/// Ends the process when a conversion outlives its budget. A parser stuck on
/// a pathological file would otherwise hold the caller's worker, and survive
/// it: closing the port does not stop a process that never reads stdin.
fn arm_watchdog(timeout: Duration) {
    thread::spawn(move || {
        thread::sleep(timeout);
        eprintln!("timed out after {}s", timeout.as_secs());
        process::exit(EXIT_TIMED_OUT);
    });
}

fn to_markdown(
    bytes: &[u8],
    format: Option<Format>,
    scanned_pages: ScannedPages,
) -> Result<Converted, Failure> {
    let pdf = format.or_else(|| Format::from_bytes(bytes)) == Some(Format::Pdf);

    match (pdf, scanned_pages) {
        (true, ScannedPages::Fail) => complete_pdf_to_markdown(bytes).map(whole),
        (true, ScannedPages::Skip) => readable_pdf_to_markdown(bytes),
        (false, _) => anydoc::to_markdown_bytes(bytes, format)
            .map(whole)
            .map_err(Failure::Convert),
    }
}

fn whole(markdown: String) -> Converted {
    Converted {
        markdown,
        unread_pages: Vec::new(),
    }
}

/// Every page of the PDF as markdown, or a failure: never part of it.
///
/// Past the scanned-page check, pdf-inspector's own answer decides. Text it
/// calls garbled fails the document: the characters are wrong, which reads
/// as complete just like a missing page does. The pages it still wants OCR
/// for are checked again by extracting them, as anydoc does, because that
/// first answer over-reports short pages.
fn complete_pdf_to_markdown(bytes: &[u8]) -> Result<String, Failure> {
    if !pages_needing_ocr(bytes)?.is_empty() {
        return Err(Failure::NeedsOcr);
    }

    let result = pdf_inspector::process_pdf_mem(bytes).map_err(pdf_failure)?;

    if result.has_encoding_issues {
        return Err(Failure::GarbledText);
    }

    if !result.pages_needing_ocr.is_empty() {
        let flagged: Vec<u32> = result
            .pages_needing_ocr
            .iter()
            .map(|page| page - 1)
            .collect();

        let without_text = pdf_inspector::extract_pages_markdown_mem(bytes, Some(&flagged))
            .map_err(pdf_failure)?
            .pages_needing_ocr;

        if !without_text.is_empty() {
            return Err(Failure::NeedsOcr);
        }
    }

    match markdown(result.markdown) {
        Some(markdown) => Ok(markdown),
        None => Err(Failure::Convert(ConvertError::Unsupported(
            "PDF has no extractable text".to_string(),
        ))),
    }
}

/// The text of the pages that have some, and the numbers of the pages that
/// are not all in it. Only a PDF with no text on any page fails. Garbled
/// text is returned as it is: this caller has no other parser to ask.
fn readable_pdf_to_markdown(bytes: &[u8]) -> Result<Converted, Failure> {
    let unread_pages = pages_needing_ocr(bytes)?;
    let result = pdf_inspector::process_pdf_mem(bytes).map_err(pdf_failure)?;

    match markdown(result.markdown) {
        Some(markdown) => Ok(Converted {
            markdown,
            unread_pages,
        }),
        None => Err(Failure::NeedsOcr),
    }
}

fn markdown(extracted: Option<String>) -> Option<String> {
    let mut markdown = extracted.filter(|markdown| !markdown.trim().is_empty())?;

    if !markdown.ends_with('\n') {
        markdown.push('\n');
    }

    Some(markdown)
}

/// The pages of a PDF whose content is in pixels, checked one by one.
///
/// anydoc asks pdf-inspector the same question with its defaults, which look
/// at eight pages and call the document text once 60% of them have some. A
/// statement of nine text pages and a scanned one passes that, and so does a
/// scan with a line of stamped text on it: anydoc converts them and the
/// markdown is missing those pages. So every page is read, the document is
/// text only when all of them are, and an image under a few text runs counts
/// as a scan.
///
/// This leans to OCR on purpose. A page of a logo and three lines goes to
/// OCR too: wasted money, where a dropped page is a wrong answer nobody sees.
fn pages_needing_ocr(bytes: &[u8]) -> Result<Vec<u32>, Failure> {
    let every_page_must_be_text = DetectionConfig {
        strategy: ScanStrategy::Full,
        min_text_ops_per_page: MIN_TEXT_OPS_ON_IMAGE_PAGE,
        text_page_ratio_threshold: 1.0,
    };

    let detected = pdf_inspector::detect_pdf_type_mem_with_config(bytes, every_page_must_be_text)
        .map_err(pdf_failure)?;

    Ok(detected
        .pages_needing_ocr
        .into_iter()
        .filter(|page| !blank(detected.ocr_reasons_by_page.get(page)))
        .collect())
}

fn blank(reasons: Option<&Vec<String>>) -> bool {
    reasons.is_some_and(|reasons| reasons.iter().all(|reason| reason == REASON_BLANK_PAGE))
}

/// As anydoc maps them, so a PDF fails the way its other formats do.
fn pdf_failure(error: PdfError) -> Failure {
    Failure::Convert(match error {
        PdfError::Encrypted => ConvertError::Encrypted,
        PdfError::Io(e) => ConvertError::Io(e),
        PdfError::NotAPdf(detail) | PdfError::Parse(detail) => {
            ConvertError::Malformed { part: None, detail }
        }
        PdfError::InvalidStructure => ConvertError::Malformed {
            part: None,
            detail: "invalid PDF structure".to_string(),
        },
    })
}

/// Only a name: `ConvertError`'s Display messages carry details that may
/// quote parts of the document.
fn failure_name(failure: &Failure) -> &'static str {
    match failure {
        Failure::NeedsOcr => "NeedsOcr",
        Failure::GarbledText => "GarbledText",
        Failure::Convert(ConvertError::Unsupported(_)) => "Unsupported",
        Failure::Convert(ConvertError::Malformed { .. }) => "Malformed",
        Failure::Convert(ConvertError::Encrypted) => "Encrypted",
        Failure::Convert(ConvertError::ResourceLimit { .. }) => "ResourceLimit",
        Failure::Convert(ConvertError::MissingPart { .. }) => "MissingPart",
        Failure::Convert(ConvertError::Io(_)) => "Io",
        Failure::Convert(_) => "Unknown",
    }
}

/// `ConvertError` is `#[non_exhaustive]`, so a variant a later anydoc adds
/// is a plain conversion error until it is given a code here.
fn exit_code(failure: &Failure) -> i32 {
    match failure {
        Failure::NeedsOcr => EXIT_NEEDS_OCR,
        Failure::GarbledText => EXIT_GARBLED_TEXT,
        Failure::Convert(ConvertError::Encrypted) => EXIT_ENCRYPTED,
        Failure::Convert(ConvertError::ResourceLimit { .. }) => EXIT_RESOURCE_LIMIT,
        Failure::Convert(_) => EXIT_CONVERT_ERROR,
    }
}
