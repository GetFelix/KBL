#!/usr/bin/env python3
"""Fold changelog.d/ fragments into CHANGELOG.md for a release.

Each pull request adds `changelog.d/<name>.<section>.md` instead of editing
CHANGELOG.md, so concurrent PRs stop conflicting on the same lines. At release
time this appends the fragments to the `## Unreleased` entries (hand-written
entries already there are kept), retitles that section as the release, leaves a
fresh empty `## Unreleased` above it, and deletes the fragments.

    changelog_assemble.py <version> [--date YYYY-MM-DD]
    changelog_assemble.py --check
"""

import argparse
import datetime
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FRAGMENT_DIR = ROOT / "changelog.d"
CHANGELOG = ROOT / "CHANGELOG.md"

SECTIONS = [
    "security",
    "added",
    "changed",
    "deprecated",
    "removed",
    "fixed",
    "documentation",
]
NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]*(\.[A-Za-z0-9_-]+)*$")


def fragment_files(directory: Path) -> list[Path]:
    if not directory.is_dir():
        return []
    return sorted(
        p for p in directory.iterdir() if p.is_file() and p.name != "README.md" and not p.name.startswith(".")
    )


def parse_name(path: Path) -> tuple[str, str] | str:
    """Return (name, section), or an error message."""
    stem, dot, ext = path.name.rpartition(".")
    if ext != "md" or not dot:
        return f"{path.name}: fragments end in .md"
    name, dot, section = stem.rpartition(".")
    if not dot or not name:
        return f"{path.name}: expected <name>.<section>.md"
    if section not in SECTIONS:
        return f"{path.name}: unknown section '{section}' (use one of {', '.join(SECTIONS)})"
    if not NAME.match(name):
        return f"{path.name}: the name may only use letters, digits, '-', '_' and '.'"
    return name, section


def load_fragments(directory: Path) -> tuple[dict[str, list[str]], list[str]]:
    by_section: dict[str, list[str]] = {s: [] for s in SECTIONS}
    errors: list[str] = []
    for path in fragment_files(directory):
        parsed = parse_name(path)
        if isinstance(parsed, str):
            errors.append(parsed)
            continue
        body = path.read_text(encoding="utf-8").strip()
        if not body:
            errors.append(f"{path.name}: empty")
            continue
        # A heading would end the Unreleased section early in the assembled file.
        if re.search(r"^#{1,3} ", body, re.MULTILINE):
            errors.append(f"{path.name}: has a heading; the file name sets the section")
            continue
        if re.match(r"- |\* ", body):
            errors.append(f"{path.name}: starts with a bullet marker; the assembler adds it")
            continue
        label = parsed[1].capitalize()
        if body.startswith(f"{label}:"):
            errors.append(f"{path.name}: starts with '{label}:'; the assembler adds the label")
            continue
        by_section[parsed[1]].append(entry(label, body))
    return by_section, errors


def entry(label: str, body: str) -> str:
    """Render a fragment as a bullet, indenting continuation lines under it."""
    first, *rest = body.splitlines()
    return "\n".join([f"- {label}: {first}"] + [f"  {l}" if l.strip() else "" for l in rest])


def split_unreleased(text: str) -> tuple[str, str, str]:
    """Split into (before, unreleased body, after the Unreleased section)."""
    found = re.search(r"^## Unreleased[ \t]*\n", text, re.MULTILINE)
    if not found:
        raise SystemExit("CHANGELOG.md has no '## Unreleased' heading")
    rest = text[found.end():]
    nxt = re.search(r"^## ", rest, re.MULTILINE)
    end = nxt.start() if nxt else len(rest)
    return text[: found.end()], rest[:end], rest[end:]


def merge(unreleased: str, fragments: dict[str, list[str]]) -> str:
    """Append fragment entries, in section order, to an Unreleased body."""
    entries = [e for s in SECTIONS for e in fragments[s]]
    return "\n".join(p for p in [unreleased.strip("\n"), *entries] if p)


def assemble(version: str, date: str) -> int:
    fragments, errors = load_fragments(FRAGMENT_DIR)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    text = CHANGELOG.read_text(encoding="utf-8")
    head, body, tail = split_unreleased(text)
    merged = merge(body, fragments)
    if not merged:
        print("nothing to release: no Unreleased entries and no fragments", file=sys.stderr)
        return 1
    if re.search(rf"^## \[{re.escape(version)}\]", text, re.MULTILINE):
        print(f"CHANGELOG.md already has a section for {version}", file=sys.stderr)
        return 1
    CHANGELOG.write_text(f"{head}\n## [{version}] - {date}\n\n{merged}\n\n{tail}", encoding="utf-8")
    for path in fragment_files(FRAGMENT_DIR):
        path.unlink()
    return 0


def check() -> int:
    _, errors = load_fragments(FRAGMENT_DIR)
    if errors:
        print("invalid changelog fragments in changelog.d/:", file=sys.stderr)
        print("\n".join(f"  {e}" for e in errors), file=sys.stderr)
        return 1
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("version", nargs="?")
    ap.add_argument("--date", default=datetime.date.today().isoformat())
    ap.add_argument("--check", action="store_true", help="validate fragments and exit")
    args = ap.parse_args()
    if args.check:
        return check()
    if not args.version:
        ap.error("a version is required unless --check is given")
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", args.date):
        ap.error("--date must be YYYY-MM-DD")
    return assemble(args.version.removeprefix("v"), args.date)


if __name__ == "__main__":
    raise SystemExit(main())
