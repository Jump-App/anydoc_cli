#![no_main]

use anydoc::Format;
use libfuzzer_sys::fuzz_target;

/// Every `--format` the CLI accepts. Index 0 of the selector byte means no
/// `--format`, so the sniffing path the CLI takes without a hint is fuzzed too.
const FORMATS: [Format; 12] = [
    Format::Doc,
    Format::Docx,
    Format::Odt,
    Format::Pdf,
    Format::Ppt,
    Format::Pptx,
    Format::Rtf,
    Format::Epub,
    Format::Excel,
    Format::Ods,
    Format::Odp,
    Format::Csv,
];

// The first byte picks the format hint; the rest is the document. This is
// the exact call `main` makes, so a panic, hang or OOM here is one the
// Elixir port would see.
fuzz_target!(|data: &[u8]| {
    let Some((&selector, document)) = data.split_first() else {
        return;
    };
    let format = match usize::from(selector) % (FORMATS.len() + 1) {
        0 => None,
        i => Some(FORMATS[i - 1]),
    };
    let _ = anydoc::to_markdown_bytes(document, format);
});
