# anydoc_cli

A thin CLI wrapper around [firecrawl/anydoc](https://github.com/firecrawl/anydoc) that
converts documents (pdf, doc/docx, ppt/pptx, xls/xlsx, odt/ods/odp, rtf, epub, csv) to
GitHub-Flavored Markdown. Used by the Jump monorepo as a document parsing sidecar: the
binary is downloaded from this repo's GitHub Releases during `mix compile` (checksum
verified) and invoked per document from an Elixir port.

## Usage

```
anydoc_cli <input-path> <output-path> [--format <fmt>]
```

- `<fmt>` is a bare extension anydoc recognizes: `pdf`, `doc`, `docx`, `ppt`, `pptx`,
  `xls`, `xlsx`, `odt`, `ods`, `odp`, `rtf`, `epub`, `csv`. When omitted, the format is
  sniffed from the file content — signature-less formats (CSV) must be named explicitly.
  The caller passes extension-less tmp paths on purpose: the format hint comes from the
  server-validated content type, never from a client-controlled filename.
- Markdown is written to `<output-path>`.
- **Document content never reaches stdout/stderr** — the Elixir port logs everything the
  process prints, and those logs are exported. Keep it that way.

Exit codes:

| Code | Meaning |
|------|---------|
| 0 | success |
| 1 | usage or I/O error |
| 2 | conversion error (`ConvertError` variant name on stderr: `Unsupported`, `Malformed`, `Encrypted`, `ResourceLimit`, `MissingPart`, `Io`) |

Scanned/image-only PDFs need OCR, which anydoc does not do — they exit `2 Unsupported`;
the caller falls back to its other parser.

## Releasing

1. Bump `version` in `Cargo.toml` (the release workflow rejects mismatched tags).
2. `git tag v<version> && git push --tags`
3. The Release workflow builds `anydoc_cli-<target>` for
   `x86_64/aarch64-unknown-linux-{gnu,musl}` and `x86_64/aarch64-apple-darwin`,
   verifies the musl binaries are fully static, and publishes them with a
   `checksums.txt` (sha256).
4. Update the pinned version in the Jump monorepo's `api/mix.exs`.
