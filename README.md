# anydoc_cli

A thin CLI wrapper around [firecrawl/anydoc](https://github.com/firecrawl/anydoc) that
converts documents (pdf, doc/docx, ppt/pptx, xls/xlsx, odt/ods/odp, rtf, epub, csv) to
GitHub-Flavored Markdown. Used by the Jump monorepo as a document parsing sidecar: the
binary is downloaded from this repo's GitHub Releases during `mix compile` (checksum
verified) and invoked per document from an Elixir port.

## Getting Started

You need Rust 1.88.0. The `rust-toolchain.toml` file pins that version, so
[rustup](https://rustup.rs) installs and picks it for you. If you use
[mise](https://mise.jdx.dev), `mise.toml` pins the same version; keep the two in
sync, because mise exports `RUSTUP_TOOLCHAIN`, which overrides `rust-toolchain.toml`.

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

`advisory-report` is a Monday-morning Slack digest, not a gate — it never runs on
a pull request and cannot block anyone. It exists because the `dependencies` gate
only fails on what a PR author can actually fix, which means a transitive
advisory with no available fix would otherwise sit in the tree unseen. The digest
uses `deny.report.toml`, which suppresses nothing. It needs a `SLACK_WEBHOOK_URL`
repository secret; the channel is whichever one the webhook was created against.

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

## Fuzzing

`fuzz/` holds a [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) target,
`convert`, that makes the same `anydoc::to_markdown_bytes` call as `main`. The
first input byte picks the format hint (one of the 12 `--format` values, or none,
so the content-sniffing path is fuzzed too); the rest is the document. For the
Elixir port a hang or an OOM is as bad as a crash, so runs set time and memory
limits.

You need a nightly toolchain (AddressSanitizer needs `-Z` flags) and cargo-fuzz:

```
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz --locked
```

Seed the corpus. Upstream anydoc's test fixtures work well; prefix each with the
selector byte for its format (0 = no hint, then 1–12 in the order of `FORMATS` in
`fuzz/fuzz_targets/convert.rs`: doc, docx, odt, pdf, ppt, pptx, rtf, epub,
xls/xlsx/xlsb, ods, odp, csv):

```
git clone --depth 1 https://github.com/firecrawl/anydoc /tmp/anydoc
mkdir -p fuzz/corpus/convert
python3 - /tmp/anydoc fuzz/corpus/convert <<'EOF'
import sys, os, hashlib
src, dst = sys.argv[1:]
idx = dict(doc=1, docx=2, odt=3, pdf=4, ppt=5, pptx=6, rtf=7, epub=8,
           xls=9, xlsx=9, xlsb=9, ods=10, odp=11, csv=12)
for root, _, files in os.walk(src):
    if '/.git' in root: continue
    for f in files:
        ext = f.rsplit('.', 1)[-1].lower()
        if ext not in idx: continue
        body = open(os.path.join(root, f), 'rb').read()
        if len(body) > 1 << 20: continue
        for sel in {idx[ext], 0}:
            data = bytes([sel]) + body
            open(os.path.join(dst, hashlib.sha1(data).hexdigest()), 'wb').write(data)
EOF
```

Run it (the first build takes ~10 minutes; `-max_total_time` is in seconds):

```
cargo +nightly fuzz run convert -O -- \
  -fork=4 -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1 \
  -rss_limit_mb=2048 -timeout=10 -max_total_time=28800
```

Findings land in `fuzz/artifacts/convert/` (`crash-*`, `timeout-*`, `oom-*`);
reproduce one with `cargo +nightly fuzz run convert -O <file>`. Almost all the
parsing is in anydoc and its dependencies, so report real bugs upstream at
[firecrawl/anydoc](https://github.com/firecrawl/anydoc). `fuzz/corpus`,
`fuzz/artifacts` and `fuzz/target` are gitignored.

The fuzz crate depends on `anydoc` directly (the CLI is a binary, not a
library), so it has its own `fuzz/Cargo.lock`. When you bump anydoc here, run
`cargo update -p anydoc --manifest-path fuzz/Cargo.toml` so the fuzzer tests the
version you ship.

## Releasing

1. Bump `version` in `Cargo.toml` (the release workflow rejects mismatched tags).
2. `git tag v<version> && git push --tags`
3. The Release workflow builds `anydoc_cli-<target>` for
   `x86_64/aarch64-unknown-linux-{gnu,musl}` and `x86_64/aarch64-apple-darwin`,
   verifies the musl binaries are fully static, and publishes them with a
   `checksums.txt` (sha256).
4. Update the pinned version in the Jump monorepo's `api/mix.exs`.
