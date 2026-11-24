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
//! with. anydoc asks it with settings a caller cannot change, which look at
//! eight pages, and it fails a PDF for garbled text only on the pages
//! pdf-inspector can name.

use std::env;
use std::fs;
use std::io;
use std::process;
use std::thread;
use std::time::Duration;

use anydoc::{ConvertError, Format};
use pdf_inspector::{
    DetectionConfig, PdfError, PdfOptions, PdfProcessResult, ScanStrategy, OCR_REASON_NO_TEXT,
};

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

/// Why a run produced no markdown. `exit` gives each one its code and what
/// it prints, and nothing else ends the process with a failure.
enum Failure {
    Usage(String),
    ReadInput(io::ErrorKind),
    WriteOutput(io::ErrorKind),
    TimedOut(Duration),
    /// A PDF with a page whose content is in pixels.
    NeedsOcr,
    /// A PDF whose text layer is not the text on the page: its fonts do not
    /// say which characters they draw, so extraction returns other ones.
    GarbledText,
    Convert(ConvertError),
}

struct Converted {
    markdown: String,
    /// The pages of a PDF whose content, or part of it, is not in the
    /// markdown as text that can be trusted: scanned pages, and pages that
    /// extracted as nothing or as garbage. Empty unless scanned pages were
    /// skipped.
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
    if let Err(failure) = run() {
        exit(&failure);
    }
}

fn run() -> Result<(), Failure> {
    let raw: Vec<String> = env::args().collect();

    let args = match parse_args(&raw) {
        Ok(args) => args,
        Err(problem) => return Err(Failure::Usage(problem)),
    };

    if let Some(timeout) = args.timeout {
        arm_watchdog(timeout);
    }

    let bytes = match fs::read(args.input_path) {
        Ok(bytes) => bytes,
        Err(error) => return Err(Failure::ReadInput(error.kind())),
    };

    let converted = to_markdown(&bytes, args.format, args.scanned_pages)?;

    // The markdown goes last, so a run that fails before it leaves none.
    if let Some(path) = args.unread_pages_out {
        write(path, &page_list(&converted.unread_pages))?;
    }

    write(args.output_path, &converted.markdown)
}

fn write(path: &str, contents: &str) -> Result<(), Failure> {
    match fs::write(path, contents) {
        Ok(()) => Ok(()),
        Err(error) => Err(Failure::WriteOutput(error.kind())),
    }
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
        let [flag, value] = pair else {
            return Err("a flag is missing its value".to_string());
        };

        match flag.as_str() {
            "--format" => match Format::from_extension(value) {
                Some(format) => parsed.format = Some(format),
                None => return Err(format!("unrecognized format: {value}")),
            },
            "--timeout-secs" => match value.parse::<u64>() {
                Ok(seconds) if seconds > 0 => parsed.timeout = Some(Duration::from_secs(seconds)),
                _ => return Err(format!("invalid timeout: {value}")),
            },
            "--scanned-pages" => match value.as_str() {
                "fail" => parsed.scanned_pages = ScannedPages::Fail,
                "skip" => parsed.scanned_pages = ScannedPages::Skip,
                _ => return Err(format!("unrecognized scanned pages mode: {value}")),
            },
            "--unread-pages-out" => parsed.unread_pages_out = Some(value),
            _ => return Err(format!("unrecognized flag: {flag}")),
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
        exit(&Failure::TimedOut(timeout));
    });
}

fn to_markdown(
    bytes: &[u8],
    format: Option<Format>,
    scanned_pages: ScannedPages,
) -> Result<Converted, Failure> {
    let pdf = format.or_else(|| Format::from_bytes(bytes)) == Some(Format::Pdf);

    if pdf {
        return match scanned_pages {
            ScannedPages::Fail => complete_pdf_to_markdown(bytes),
            ScannedPages::Skip => readable_pdf_to_markdown(bytes),
        };
    }

    match anydoc::to_markdown_bytes(bytes, format) {
        Ok(markdown) => Ok(Converted {
            markdown,
            unread_pages: Vec::new(),
        }),
        Err(error) => Err(Failure::Convert(error)),
    }
}

/// Every page of the PDF as markdown, or a failure: never part of it.
///
/// The two PDF modes run the same steps (`pages_needing_ocr`, `extract`,
/// `pages_without_text`, `converted`) and differ only in what a page that
/// cannot be read does: here it fails the document, there it is named.
///
/// A scanned page fails before anything is extracted, since no extraction
/// makes the document whole. Garbled text fails next: the characters are
/// wrong, which reads as complete just like a missing page does.
fn complete_pdf_to_markdown(bytes: &[u8]) -> Result<Converted, Failure> {
    if !pages_needing_ocr(bytes)?.is_empty() {
        return Err(Failure::NeedsOcr);
    }

    let extracted = extract(bytes)?;

    if extracted.has_encoding_issues {
        return Err(Failure::GarbledText);
    }

    if !pages_without_text(bytes, &extracted.pages_needing_ocr, &[])?.is_empty() {
        return Err(Failure::NeedsOcr);
    }

    converted(extracted.markdown, Vec::new())
}

/// The text of the pages that have some, and the numbers of the pages that
/// are not all in it. Garbled text is returned as it is, with its pages
/// named where pdf-inspector can say which they are: this caller has no
/// other parser to ask.
fn readable_pdf_to_markdown(bytes: &[u8]) -> Result<Converted, Failure> {
    let mut unread_pages = pages_needing_ocr(bytes)?;
    let extracted = extract(bytes)?;

    unread_pages.extend(pages_without_text(
        bytes,
        &extracted.pages_needing_ocr,
        &unread_pages,
    )?);
    unread_pages.sort_unstable();

    converted(extracted.markdown, unread_pages)
}

/// pdf-inspector's extraction, with every page looked at before it decides
/// what the document is. Its default looks at eight, and a document it takes
/// for a scan gets no markdown at all: twenty scanned pages with a typed
/// letter on the second would come back empty.
fn extract(bytes: &[u8]) -> Result<PdfProcessResult, Failure> {
    let every_page_looked_at = DetectionConfig {
        strategy: ScanStrategy::Full,
        ..DetectionConfig::default()
    };

    pdf_inspector::process_pdf_mem_with_options(
        bytes,
        PdfOptions::new().detection(every_page_looked_at),
    )
    .map_err(pdf_failure)
}

/// Of the pages the extraction flagged, the ones that hold no text to trust
/// when extracted on their own, as anydoc checks them: the extraction's own
/// list over-reports short pages. `known` are pages already counted as
/// unread, which are not asked about twice.
///
/// An empty page comes back from this too, when the document is so short
/// that pdf-inspector flags all of it. Its answer does not tell an empty
/// page from one whose text it threw away as garbage, so both count.
fn pages_without_text(bytes: &[u8], flagged: &[u32], known: &[u32]) -> Result<Vec<u32>, Failure> {
    let unchecked: Vec<u32> = flagged
        .iter()
        .filter(|page| !known.contains(page))
        .map(|page| page - 1)
        .collect();

    if unchecked.is_empty() {
        return Ok(Vec::new());
    }

    match pdf_inspector::extract_pages_markdown_mem(bytes, Some(&unchecked)) {
        Ok(pages) => Ok(pages.pages_needing_ocr),
        Err(error) => Err(pdf_failure(error)),
    }
}

/// One answer for a PDF that gave no text, whichever mode asked: it needs
/// OCR when pages of it could not be read, and has nothing to convert when
/// none are.
fn converted(extracted: Option<String>, unread_pages: Vec<u32>) -> Result<Converted, Failure> {
    match (markdown(extracted), unread_pages.is_empty()) {
        (Some(markdown), _) => Ok(Converted {
            markdown,
            unread_pages,
        }),
        (None, false) => Err(Failure::NeedsOcr),
        (None, true) => Err(Failure::Convert(ConvertError::Unsupported(
            "PDF has no extractable text".to_string(),
        ))),
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
/// at eight pages and go through the rest only when one of those eight
/// carries an image. A statement whose one scanned page is not among the
/// eight passes that, and so does a scan with a line of stamped text on it:
/// anydoc converts them and the markdown is missing those pages. So every
/// page is read, the document is text only when all of them are, and an
/// image under a few text runs counts as a scan.
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

/// A page flagged only for having no text and no image is blank. Nothing on
/// it for OCR to read, so it does not make a document need OCR.
fn blank(reasons: Option<&Vec<String>>) -> bool {
    reasons.is_some_and(|reasons| reasons.iter().all(|reason| reason == OCR_REASON_NO_TEXT))
}

/// To the variants anydoc maps them to, so a PDF fails the way its other
/// formats do.
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

/// The one place a failure gets its exit code and what it prints.
///
/// A conversion error prints only its name: `ConvertError`'s Display
/// messages carry details that may quote parts of the document.
///
/// `ConvertError` is `#[non_exhaustive]`, so a variant a later anydoc adds
/// is a plain conversion error until it is given an arm here.
fn exit(failure: &Failure) -> ! {
    let (code, message) = match failure {
        Failure::Usage(problem) => (EXIT_USAGE_OR_IO, format!("{problem}\n{USAGE}")),
        Failure::ReadInput(kind) => (EXIT_USAGE_OR_IO, format!("failed to read input: {kind}")),
        Failure::WriteOutput(kind) => (EXIT_USAGE_OR_IO, format!("failed to write output: {kind}")),
        Failure::TimedOut(timeout) => (
            EXIT_TIMED_OUT,
            format!("timed out after {}s", timeout.as_secs()),
        ),
        Failure::NeedsOcr => (EXIT_NEEDS_OCR, convert_error("NeedsOcr")),
        Failure::GarbledText => (EXIT_GARBLED_TEXT, convert_error("GarbledText")),
        Failure::Convert(ConvertError::NeedsOcr { .. }) => {
            (EXIT_NEEDS_OCR, convert_error("NeedsOcr"))
        }
        Failure::Convert(ConvertError::Encrypted) => (EXIT_ENCRYPTED, convert_error("Encrypted")),
        Failure::Convert(ConvertError::ResourceLimit { .. }) => {
            (EXIT_RESOURCE_LIMIT, convert_error("ResourceLimit"))
        }
        Failure::Convert(ConvertError::Unsupported(_)) => {
            (EXIT_CONVERT_ERROR, convert_error("Unsupported"))
        }
        Failure::Convert(ConvertError::Malformed { .. }) => {
            (EXIT_CONVERT_ERROR, convert_error("Malformed"))
        }
        Failure::Convert(ConvertError::MissingPart { .. }) => {
            (EXIT_CONVERT_ERROR, convert_error("MissingPart"))
        }
        Failure::Convert(ConvertError::Io(_)) => (EXIT_CONVERT_ERROR, convert_error("Io")),
        Failure::Convert(_) => (EXIT_CONVERT_ERROR, convert_error("Unknown")),
    };

    eprintln!("{message}");
    process::exit(code);
}

fn convert_error(name: &str) -> String {
    format!("convert error: {name}")
}
