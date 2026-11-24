#!/usr/bin/env python3
"""Check that every tracked Markdown file is reachable from README.md and that
every relative link and heading anchor resolves."""

import collections
import re
import subprocess
import sys
import unicodedata
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent.parent
FENCE = re.compile(r"^ {0,3}(```|~~~)")
INLINE_CODE = re.compile(r"(`+)(?:(?!\1).)+?\1")
INLINE_LINK = re.compile(r"!?\[(?:[^\[\]]|\[[^\]]*\])*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
REF_DEF = re.compile(r"^ {0,3}\[[^\]]+\]:\s*<?(\S+?)>?(?:\s+.*)?$")
HTML_HREF = re.compile(r"<a\s[^>]*href=\"([^\"]+)\"", re.I)
HTML_ID = re.compile(r"<a\s[^>]*(?:id|name)=\"([^\"]+)\"", re.I)
HEADING = re.compile(r"^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$")


def tracked_markdown():
    out = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.md"], cwd=ROOT, check=True, capture_output=True
    ).stdout.decode()
    return sorted(PurePosixPath(p) for p in out.split("\0") if p)


def prose_lines(text):
    """Yield (line_number, line) outside fenced code blocks."""
    fence = None
    for number, line in enumerate(text.splitlines(), 1):
        match = FENCE.match(line)
        if match:
            if fence is None:
                fence = match.group(1)
            elif match.group(1) == fence:
                fence = None
            continue
        if fence is None:
            yield number, line


def slugify(heading):
    text = INLINE_LINK.sub(lambda m: re.sub(r"^!?\[|\]\(.*$", "", m.group(0)), heading)
    text = re.sub(r"<[^>]+>", "", text).lower()
    kept = []
    for ch in text:
        if ch in " -_" or unicodedata.category(ch)[0] in "LN":
            kept.append("-" if ch == " " else ch)
    return "".join(kept)


def anchors(text):
    found = set()
    seen = collections.Counter()
    for _, line in prose_lines(text):
        found.update(HTML_ID.findall(line))
        match = HEADING.match(line)
        if not match:
            continue
        slug = slugify(match.group(2))
        found.add(slug if seen[slug] == 0 else f"{slug}-{seen[slug]}")
        seen[slug] += 1
    return found


def links(text):
    for number, line in prose_lines(text):
        line = INLINE_CODE.sub("", line)
        targets = INLINE_LINK.findall(line) + HTML_HREF.findall(line)
        ref = REF_DEF.match(line)
        if ref:
            targets.append(ref.group(1))
        for target in targets:
            yield number, target


def main():
    files = tracked_markdown()
    texts = {path: (ROOT / path).read_text(encoding="utf-8") for path in files}
    anchor_cache = {}
    errors = []
    graph = {}
    for path, text in texts.items():
        edges = []
        for number, target in links(text):
            if re.match(r"^[a-z][a-z0-9+.-]*:", target, re.I):
                continue
            raw_path, _, fragment = target.partition("#")
            resolved = (ROOT / (path.parent / raw_path if raw_path else path)).resolve()
            where = f"{path}:{number}"
            if not resolved.exists():
                errors.append(f"{where}: broken link {target}")
                continue
            rel = PurePosixPath(resolved.relative_to(ROOT).as_posix())
            if resolved.is_dir() and (resolved / "README.md").exists():
                rel = rel / "README.md"
            if rel.suffix == ".md":
                edges.append(rel)
                if fragment:
                    if rel not in anchor_cache:
                        anchor_cache[rel] = anchors(texts.get(rel) or (ROOT / rel).read_text())
                    if fragment not in anchor_cache[rel]:
                        errors.append(f"{where}: missing anchor {target}")
        graph[path] = edges

    reached = set()
    queue = collections.deque([PurePosixPath("README.md")])
    while queue:
        path = queue.popleft()
        if path in reached:
            continue
        reached.add(path)
        queue.extend(graph.get(path, []))
    for path in files:
        if path not in reached:
            errors.append(f"{path}: not reachable from README.md")

    for error in errors:
        print(error)
    if errors:
        print(f"{len(errors)} documentation link error(s)", file=sys.stderr)
        return 1
    print(f"{len(files)} Markdown files reachable; all relative links resolve")
    return 0


if __name__ == "__main__":
    sys.exit(main())
