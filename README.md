# anydoc_cli

A thin CLI wrapper around [firecrawl/anydoc](https://github.com/firecrawl/anydoc) that
converts documents (pdf, doc/docx, ppt/pptx, xls/xlsx, odt/ods/odp, rtf, epub, csv) to
GitHub-Flavored Markdown. Used by the Jump monorepo as a document parsing sidecar: the
binary is downloaded from this repo's GitHub Releases during `mix compile` (checksum
verified) and invoked per document from an Elixir port.

## Getting Started

You need Rust 1.88.0. The `rust-toolchain.toml` file pins that version, so
[rustup](https://rustup.rs) installs and picks it for you.

Build and test:

```
git clone https://github.com/Jump-App/anydoc_cli.git
cd anydoc_cli
cargo build          # debug build
cargo test           # CLI integration tests
```

Try it on a small file:

```
printf 'name,balance\nAcme,100.50\n' > sample.csv
cargo run -- sample.csv out.md --format csv
cat out.md
```

Before you open a pull request, run the same checks CI runs:

```
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test
cargo build --locked --release
cargo deny check all          # needs: cargo install cargo-deny --locked
```

Two of these are easy to skip and shouldn't be. `cargo test` builds without LTO,
so only a release build proves the binary still links. And `cargo deny` is the
check that matters most here: this tool parses untrusted documents, and almost
all of that parsing happens in dependencies, so the dependency tree is the real
attack surface. Its policy lives in `deny.toml`.

CI runs these across five workflows: `lint`, `test`, `dependencies` (cargo-deny,
also nightly, because advisories are published against code that never changed),
`codeql` (static analysis, results in the repo's Security tab), and
`advisory-report`.

`advisory-report` is a Monday-morning Slack digest, not a gate: it never runs on
a pull request and cannot block anyone. It exists because the `dependencies` gate
only fails on what a PR author can actually fix, which means a transitive
advisory with no available fix would otherwise sit in the tree unseen. The digest
uses `deny.report.toml`, which suppresses nothing. It needs a `SLACK_WEBHOOK_URL`
repository secret; the channel is whichever one the webhook was created against.

## Usage

```
anydoc_cli <input-path> <output-path> [--format <fmt>] [--timeout-secs <n>] [--scanned-pages <fail|skip>] [--unread-pages-out <path>]
```

- `<fmt>` is a bare extension anydoc recognizes: `pdf`, `doc`, `docx`, `ppt`, `pptx`,
  `xls`, `xlsx`, `odt`, `ods`, `odp`, `rtf`, `epub`, `csv`. When omitted, the format is
  sniffed from the file content, so signature-less formats (CSV) must be named explicitly.
  The caller passes extension-less tmp paths on purpose: the format hint comes from the
  server-validated content type, never from a client-controlled filename.
- `--timeout-secs` ends the process with exit code 4 when the run takes longer. Pass it
  whenever the input is untrusted: a parser stuck on a pathological file otherwise runs
  until something kills it, and closing the caller's port does not.
- `--scanned-pages` says what a PDF's scanned pages do to the conversion. `fail`, the
  default, refuses the whole document with exit code 3 (see below): for a caller with an
  OCR parser to fall back to. `skip` converts the pages that have text and leaves the
  rest out, failing only a PDF with no text at all: for a caller with nothing to fall
  back to, which would rather read most of a document than none of it.
- `--unread-pages-out` names a file that gets the numbers of the PDF pages whose content,
  or part of it, is in pixels and so is not in the markdown: counted from 1, comma
  separated, written next to the markdown and empty when every page is in. A caller that
  skips scanned pages passes it so it can say which pages it did not read.
- Markdown is written to `<output-path>`, and only on exit code 0.
- **Document content never reaches stdout/stderr**: the Elixir port logs everything the
  process prints, and those logs are exported. Keep it that way.

Exit codes are the whole contract: the caller cannot read what the process prints, so
every outcome it treats differently has its own code.

| Code | Meaning |
|------|---------|
| 0 | success: the markdown holds every page (with `--scanned-pages skip`, every page that has text) |
| 1 | usage or I/O error |
| 2 | conversion error (`ConvertError` variant name on stderr: `Unsupported`, `Malformed`, `MissingPart`, `Io`, or `Unknown` for a variant this build does not name) |
| 3 | needs OCR: a PDF with at least one page whose content is in pixels (with `--scanned-pages skip`, with none that has text) |
| 4 | timed out (`--timeout-secs`) |
| 5 | encrypted: a password is needed to open it |
| 6 | resource limit: one of anydoc's fixed safety limits (decompression, nesting, node count) |
| 7 | garbled text: a PDF whose text layer is not the text on its pages (never with `--scanned-pages skip`) |

### PDFs that need OCR

anydoc does no OCR. A PDF with a scanned page must fail whole, because markdown that is
missing a page reads as complete and nobody downstream can tell.

anydoc's own check is not strict enough for that. It asks pdf-inspector with the
defaults, which sample eight pages and call the document text once 60% of them have
some: a statement of nine text pages and one scanned page converts, without the scanned
page. A scan with a few lines of text printed over it (an envelope id, a footer, a page
number) converts too, as those lines.

So before converting a PDF, this CLI runs pdf-inspector's detection itself with every
page read, a document counted as text only when all of its pages are, and a page that
carries an image trusted to its text only from ten text runs up (`pages_needing_ocr` in
`src/main.rs`). Any page that fails is exit code 3, or with `--scanned-pages skip` a
page left out and named. A blank page is not one of them: there is nothing on it to
read.

It leans to OCR on purpose. A cover page of a logo and three lines sends its document to
OCR as well, which costs the caller a paid parse; a dropped page costs a wrong answer.
`tests/integration.rs` pins each case, so an anydoc or pdf-inspector bump that moves one
fails there.

### PDFs with garbled text

A PDF can carry a text layer that is not the text on the page: a font subset whose
character map is broken extracts as other letters, symbols or `Word$Word$Word`. It is
all there and none of it is right, which reads as complete just like a missing page.
pdf-inspector detects it (replacement characters, private-use runs, `$` for spaces,
letter statistics of a substitution cipher) and anydoc only logs a warning, so this CLI
reads the flag itself and exits 7. OCR reads such a file correctly, since the pixels are
fine. With `--scanned-pages skip` the text is returned as it is: that caller has no
other parser to ask.

The detection has a floor. A few garbled lines on a page of good text (two or three
lines in a broken font) stay under pdf-inspector's thresholds and convert with those
lines wrong. It also has no ceiling: one replacement character anywhere fails the whole
document, which is the same lean to OCR as the scan check.

That is why a PDF goes to pdf-inspector directly here and not through anydoc: which
pages are scanned and whether the text is garbled are answers anydoc does not pass on.
`complete_pdf_to_markdown` in `src/main.rs` otherwise follows anydoc's PDF path, except
that it looks for garbled text before it extracts the flagged pages again.

What still converts with content missing, because nothing in pdf-inspector's public API
tells it apart from a page that is fine:

- a scanned page under ten or more text runs, such as a scanned form with typed-in
  answers (the answers come out, the form's own wording does not) or a scan that carries
  its own OCR layer (that layer is used as is)
- text drawn inside an image on a page that otherwise has plenty of text, such as a
  chart or a screenshot
- pictures in the other formats (a scan pasted into a Word file comes out as its alt text)

## Releasing

1. Bump `version` in `Cargo.toml` (the release workflow rejects mismatched tags).
2. `git tag v<version> && git push --tags`
3. The Release workflow builds `anydoc_cli-<target>` for
   `x86_64/aarch64-unknown-linux-{gnu,musl}` and `x86_64/aarch64-apple-darwin`,
   verifies the musl binaries are fully static, and publishes them with a
   `checksums.txt` (sha256).
4. Update the pinned version in the Jump monorepo's `api/mix.exs`.
