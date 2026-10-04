#!/usr/bin/env python3
"""Check the book's internal links and includes (`--src <dir>` for another book): every chapter named in
SUMMARY.md exists, every relative `[text](file.md)` or `[text](file.md#x)`
link in a chapter points at a file that exists, and every
`{{#include path}}` resolves. External links are not fetched.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_SRC = ROOT / "docs/book/src"
LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)\)")
INCLUDE = re.compile(r"\{\{#include\s+([^}\s]+)\}\}")


def main() -> int:
    args = sys.argv[1:]
    src = Path(args[args.index("--src") + 1]).resolve() if "--src" in args else DEFAULT_SRC
    problems = []
    summary = src / "SUMMARY.md"
    if not summary.exists():
        print(f"{summary} is missing", file=sys.stderr)
        return 1
    chapters = [src / target for target in LINK.findall(summary.read_text())]
    for chapter in chapters:
        if not chapter.exists():
            problems.append(f"SUMMARY.md names {chapter.name}, which does not exist")
    for chapter in sorted(src.glob("*.md")):
        text = chapter.read_text()
        for target in LINK.findall(text):
            if target.startswith(("http://", "https://", "mailto:", "#")):
                continue
            path = (chapter.parent / target.split("#", 1)[0]).resolve()
            if not path.exists():
                problems.append(f"{chapter.name}: link to {target} does not resolve")
        for target in INCLUDE.findall(text):
            path = (chapter.parent / target.split(":", 1)[0]).resolve()
            if not path.exists():
                problems.append(f"{chapter.name}: include of {target} does not resolve")
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        return 1
    print(f"book links: {len(chapters)} chapters, every link and include resolves")
    return 0


if __name__ == "__main__":
    sys.exit(main())
