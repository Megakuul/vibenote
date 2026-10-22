#!/usr/bin/env python3
"""The next release from conventional commits (used by .github/workflows/release.yml).

Reads the commits since the last `v<major>.<minor>.<patch>` tag on HEAD's history (all commits if
there is none, starting from 0.0.0) and prints the next tag, or nothing if no commit asks for a
release:

  fix: / perf:                      → patch
  feat:                             → minor
  type!: or a BREAKING CHANGE: body → major
  anything else (docs, chore, ci, refactor, test, style, build, merges, non-conventional) → none

Usage: next-version.py [--changelog FILE] [--repo DIR]
With $GITHUB_OUTPUT set, also writes `tag=<tag>` (empty for no release) and `previous=<tag>`.
"""

import argparse
import os
import re
import subprocess
import sys

HEADER = re.compile(r"^(?P<type>[A-Za-z]+)(?:\((?P<scope>[^)]*)\))?(?P<bang>!)?:\s*(?P<desc>.+)$")
BREAKING = re.compile(r"^BREAKING[ -]CHANGE:", re.M)
LEVEL = {"fix": 1, "perf": 1, "feat": 2}
SECTIONS = [
    ("breaking", "Breaking changes"),
    ("feat", "Features"),
    ("fix", "Bug fixes"),
    ("perf", "Performance"),
]


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], check=True, capture_output=True, text=True).stdout


def last_tag(repo):
    try:
        return git(repo, "describe", "--tags", "--abbrev=0", "--match", "v[0-9]*.[0-9]*.[0-9]*", "HEAD").strip()
    except subprocess.CalledProcessError:
        return None


def parse_version(tag):
    m = re.fullmatch(r"v(\d+)\.(\d+)\.(\d+)", tag)
    if not m:
        raise SystemExit(f"tag {tag} is not v<major>.<minor>.<patch>")
    return tuple(int(x) for x in m.groups())


def commits(repo, since):
    span = f"{since}..HEAD" if since else "HEAD"
    raw = git(repo, "log", "--no-merges", "--format=%h%x00%s%x00%b%x1e", span)
    for entry in raw.split("\x1e"):
        entry = entry.strip("\n")
        if not entry:
            continue
        sha, subject, body = (entry.split("\x00") + ["", ""])[:3]
        yield sha, subject.strip(), body


def classify(subject, body):
    """(level, section, description): level 0 none, 1 patch, 2 minor, 3 major."""
    m = HEADER.match(subject)
    if not m:
        return 0, None, subject
    kind = m.group("type").lower()
    desc = m.group("desc").strip()
    if m.group("scope"):
        desc = f"**{m.group('scope')}:** {desc}"
    if m.group("bang") or BREAKING.search(body):
        return 3, "breaking", desc
    return LEVEL.get(kind, 0), kind if kind in LEVEL else None, desc


def bump(version, level):
    major, minor, patch = version
    if level == 3:
        return (major + 1, 0, 0)
    if level == 2:
        return (major, minor + 1, 0)
    return (major, minor, patch + 1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--changelog", help="write the release notes (Markdown) here")
    ap.add_argument("--repo", default=".")
    a = ap.parse_args()

    prev = last_tag(a.repo)
    base = parse_version(prev) if prev else (0, 0, 0)
    level = 0
    notes = {key: [] for key, _ in SECTIONS}
    for sha, subject, body in commits(a.repo, prev):
        lv, section, desc = classify(subject, body)
        level = max(level, lv)
        if section:
            notes[section].append(f"- {desc} ({sha})")

    tag = "v%d.%d.%d" % bump(base, level) if level else ""
    if a.changelog:
        with open(a.changelog, "w") as f:
            for key, title in SECTIONS:
                if notes[key]:
                    f.write(f"### {title}\n\n" + "\n".join(notes[key]) + "\n\n")
    out = os.environ.get("GITHUB_OUTPUT")
    if out:
        with open(out, "a") as f:
            f.write(f"tag={tag}\nprevious={prev or ''}\n")
    print(tag or "no release", file=sys.stderr if not tag else sys.stdout)


if __name__ == "__main__":
    main()
