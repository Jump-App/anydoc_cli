#!/usr/bin/env python3
"""Turn `cargo deny --format json check advisories` output into a Slack message.

Reads the NDJSON diagnostic stream on stdin, writes an incoming-webhook payload
to stdout. Always exits 0 when it produced a payload: this is a reporter, and a
week with findings is a normal result, not a failure. It exits non-zero only if
the input was unusable, which means the digest is wrong rather than merely
unwelcome.

Usage:
    cargo deny --config deny.report.toml --format json check advisories 2>&1 \
        | python3 .github/scripts/advisory_report.py > payload.json
"""

import json
import os
import sys

# Slack rejects a payload over 50 blocks, and truncates a section's text past
# 3000 characters. Cap well under both and say what was dropped: a digest that
# silently truncates reads as "that is all of them" when it is not.
MAX_ITEMS = 15
MAX_PATH_NODES = 6

# `informational` is absent on a real vulnerability and set on everything else.
KIND_LABEL = {
    "unmaintained": "Unmaintained",
    "unsound": "Unsound",
    "notice": "Notice",
}


def parse(stream):
    """Collect advisory diagnostics, keyed by ID so repeats collapse."""
    found = {}
    saw_summary = False

    for line in stream:
        line = line.strip()
        if not line or not line.startswith("{"):
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue

        if record.get("type") == "summary":
            saw_summary = True
            continue
        if record.get("type") != "diagnostic":
            continue

        fields = record.get("fields", {})
        advisory = fields.get("advisory")
        if not advisory:
            # `yanked` diagnostics carry no advisory object; synthesise one so
            # a yanked crate still reaches the digest instead of vanishing.
            label = fields.get("message", "unknown issue")
            found.setdefault(
                f"{fields.get('code', 'unknown')}:{label}",
                {
                    "id": fields.get("code", "unknown"),
                    "title": label,
                    "package": "",
                    "kind": fields.get("code", "notice"),
                    "url": "",
                    "cvss": None,
                    "paths": paths_from(fields.get("graphs", [])),
                },
            )
            continue

        found.setdefault(
            advisory["id"],
            {
                "id": advisory["id"],
                "title": advisory.get("title", ""),
                "package": advisory.get("package", ""),
                "kind": advisory.get("informational") or "vulnerability",
                "url": advisory.get("url") or "",
                "cvss": advisory.get("cvss"),
                "paths": paths_from(fields.get("graphs", [])),
            },
        )

    if not saw_summary:
        raise SystemExit(
            "advisory_report: no cargo-deny summary record found; the scan did "
            "not run to completion, so this digest would understate the results."
        )

    return list(found.values())


def paths_from(graphs):
    """Flatten cargo-deny's inclusion graph into 'root -> ... -> affected'.

    Knowing a crate arrived via lopdf rather than directly is what tells a
    reader whether they can act on it, so it is worth carrying into Slack.
    """
    chains = []

    def walk(node, trail):
        krate = node.get("Krate") or {}
        trail = [krate.get("name", "?")] + trail

        # cargo-deny marks a node `repeat` when that subtree was already
        # emitted elsewhere in the graph. It is still a genuine second way the
        # crate enters the tree, so keep it, but flag it as truncated rather
        # than passing off a two-node stub as the whole route.
        if node.get("repeat"):
            chains.append((False, ["…"] + trail))
            return

        parents = node.get("parents")
        if not parents:
            chains.append((True, trail))
            return
        for parent in parents:
            walk(parent, trail)

    for graph in graphs:
        walk(graph, [])

    # Complete chains first (they name the root that pulled the crate in), then
    # shortest, since the most direct route is the most actionable.
    chains.sort(key=lambda c: (not c[0], len(c[1])))
    return [trail for _, trail in chains]


def render_path(chain):
    if len(chain) > MAX_PATH_NODES:
        chain = chain[:2] + ["…"] + chain[-(MAX_PATH_NODES - 3):]
    return " → ".join(f"`{c}`" if c != "…" else c for c in chain)


def describe(item):
    bits = []
    if item["url"]:
        bits.append(f"<{item['url']}|{item['id']}>")
    else:
        bits.append(item["id"])

    head = f"*{item['package']}* — {item['title']}" if item["package"] else item["title"]
    lines = [head, " · ".join(bits)]

    if item["cvss"]:
        lines[1] += f" · CVSS {item['cvss']}"
    if item["paths"]:
        lines.append(render_path(item["paths"][0]))
        extra = len(item["paths"]) - 1
        if extra:
            lines.append(f"_and {extra} other path{'s' if extra > 1 else ''} into the tree_")

    return "\n".join(lines)


def build(items, repo, run_url, config):
    vulns = [i for i in items if i["kind"] == "vulnerability"]
    others = [i for i in items if i["kind"] != "vulnerability"]

    blocks = [
        {
            "type": "header",
            "text": {"type": "plain_text", "text": f"Dependency advisories — {repo}"},
        }
    ]

    if not items:
        blocks.append(
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": (
                        "*No open advisories.* Every crate in `Cargo.lock` is clear "
                        "of the RustSec database this week."
                    ),
                },
            }
        )
        summary = f"{repo}: no open dependency advisories"
    else:
        parts = []
        if vulns:
            parts.append(f"{len(vulns)} vulnerabilit{'y' if len(vulns) == 1 else 'ies'}")
        if others:
            parts.append(f"{len(others)} informational")
        summary = f"{repo}: {', '.join(parts)}"

        blocks.append(
            {
                "type": "section",
                "text": {"type": "mrkdwn", "text": f"*{summary}.*"},
            }
        )

        shown = 0
        for title, group in (("Vulnerabilities", vulns), ("Informational", others)):
            if not group:
                continue
            blocks.append({"type": "divider"})
            blocks.append(
                {
                    "type": "section",
                    "text": {"type": "mrkdwn", "text": f"*{title}* ({len(group)})"},
                }
            )
            for item in group:
                if shown >= MAX_ITEMS:
                    break
                prefix = KIND_LABEL.get(item["kind"], "")
                text = describe(item)
                if prefix:
                    text = f"{prefix} · {text}"
                blocks.append(
                    {"type": "section", "text": {"type": "mrkdwn", "text": text}}
                )
                shown += 1

        dropped = len(items) - shown
        if dropped > 0:
            blocks.append(
                {
                    "type": "section",
                    "text": {
                        "type": "mrkdwn",
                        "text": (
                            f"_{dropped} further advisor{'y' if dropped == 1 else 'ies'} "
                            f"not shown — see the full run._"
                        ),
                    },
                }
            )

    context = f"Weekly scan · policy `{config}`"
    if run_url:
        context += f" · <{run_url}|workflow run>"
    blocks.append(
        {"type": "context", "elements": [{"type": "mrkdwn", "text": context}]}
    )

    # `text` is the notification/fallback line, shown where blocks are not.
    return {"text": summary, "blocks": blocks}


def main():
    items = parse(sys.stdin)
    repo = os.environ.get("GITHUB_REPOSITORY", "anydoc_cli")
    config = os.environ.get("DENY_CONFIG", "deny.report.toml")

    run_url = ""
    server = os.environ.get("GITHUB_SERVER_URL")
    run_id = os.environ.get("GITHUB_RUN_ID")
    if server and run_id:
        run_url = f"{server}/{repo}/actions/runs/{run_id}"

    json.dump(build(items, repo, run_url, config), sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
