#!/usr/bin/env python3
"""Report Claude Code CLI surface the SDK does not cover.

Compares the installed `claude` binary against `src/`:
  - every flag `claude --help` lists must appear in src/ as a "--flag" literal
  - every public (non-@internal) control-request subtype in the binary's
    schemas must appear in src/ as a "subtype" string literal
  - every public field of those requests, and of the user-message schema,
    must appear in src/ as a "wire_name" literal or a snake_case identifier
    (`subtype.field` / `user.field` in the report)
Entries listed in scripts/cli-parity-ignore.txt (with a reason) are skipped.
A name found in src/ only proves the SDK mentions it, not that it is wired
correctly; live runs verify behaviour.

Usage: python3 -I scripts/cli-parity.py   (exit 1 when gaps remain)
Run it after each Claude Code update.
"""
import pathlib
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def ignored():
    names = set()
    for line in (ROOT / "scripts" / "cli-parity-ignore.txt").read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            names.add(line.split()[0])
    return names


def binary_text():
    claude = shutil.which("claude")
    if not claude:
        sys.exit("claude not found on PATH")
    path = pathlib.Path(claude).resolve()
    return path.read_bytes().decode("latin-1")


def object_end(text, start):
    """Index of the `}` closing the object literal whose `{` is at/after start."""
    i = text.find("{", start) + 1
    depth, quote = 0, None
    while i < len(text):
        ch = text[i]
        if quote:
            if ch == "\\":
                i += 2
                continue
            if ch == quote:
                quote = None
        elif ch in "\"'`":
            quote = ch
        elif ch in "({[":
            depth += 1
        elif ch in ")}]":
            if depth == 0:
                return i
            depth -= 1
        i += 1
    return i


def top_level_fields(text, start):
    """(name, segment) of each top-level key of the object literal at start."""
    i = text.find("{", start) + 1
    depth, quote, token, out, current, seg_start = 0, None, "", [], None, i
    while i < len(text):
        ch = text[i]
        if quote:
            if ch == "\\":
                i += 2
                continue
            if ch == quote:
                quote = None
        elif ch in "\"'`":
            quote = ch
        elif ch in "({[":
            depth += 1
        elif ch in ")}]":
            if depth == 0:
                break
            depth -= 1
        elif depth == 0:
            if ch == ":" and re.fullmatch(r"\s*[A-Za-z_$][\w$]*", token):
                if current:
                    out.append((current, text[seg_start:i]))
                current, token, seg_start = token.strip(), "", i
            elif ch == ",":
                token = ""
            else:
                token += ch
            i += 1
            continue
        i += 1
    if current:
        out.append((current, text[seg_start:i]))
    return out


def public_fields(text, start):
    fields = []
    for name, segment in top_level_fields(text, start):
        describes = re.findall(r'\.describe\(["\'`](.{0,9})', segment)
        if name in ("subtype", "type") or (describes and describes[-1].startswith("@internal")):
            continue
        fields.append(name)
    return fields


def snake(name):
    return re.sub(r"(?<=[a-z0-9])([A-Z])", r"_\1", name).lower()


def covered(src, name):
    """A quoted wire name (json! keys, serde renames) or a `pub` field."""
    return f'"{name}"' in src or re.search(rf"\bpub {re.escape(snake(name))}\b", src) is not None


def public_subtypes(text):
    subtypes = {}
    for m in re.finditer(r'c\(\{subtype:[\w$]+\("([a-z_]+)"\)', text):
        end = object_end(text, m.start() + 2)
        tail = text[end:end + 40]
        internal = re.match(r'\}\)\.describe\(["\'`]@internal', tail) is not None
        subtypes[m.group(1)] = subtypes.get(m.group(1), False) or not internal
    return sorted(name for name, public in subtypes.items() if public)


def main():
    src = "\n".join(p.read_text() for p in (ROOT / "src").rglob("*.rs"))
    skip = ignored()
    help_text = subprocess.run(["claude", "--help"], capture_output=True, text=True).stdout
    flags = sorted(set(re.findall(r"^\s+(?:-\w, )?(--[a-z][a-z0-9-]+)", help_text, re.M)))
    version = subprocess.run(["claude", "--version"], capture_output=True, text=True).stdout.strip()

    missing_flags = [f for f in flags if f'"{f}"' not in src and f not in skip]
    text = binary_text()
    subtypes = public_subtypes(text)
    missing_subtypes = [s for s in subtypes if f'"{s}"' not in src and s not in skip]
    targets = [(s, text.find(f'c({{subtype:v("{s}")')) for s in subtypes if s not in skip]
    targets.append(("user", text.find('c({type:v("user"),message:')))
    missing_fields = [
        f"{owner}.{field}"
        for owner, start in targets if start >= 0
        for field in public_fields(text, start)
        if not covered(src, field) and f"{owner}.{field}" not in skip
    ]

    print(f"claude {version}")
    print(f"flags: {len(flags)} listed, {len(missing_flags)} not covered")
    for f in missing_flags:
        print(f"  {f}")
    print(f"control requests: {len(missing_subtypes)} public subtypes not covered")
    for s in missing_subtypes:
        print(f"  {s}")
    print(f"fields: {len(missing_fields)} public fields not covered")
    for f in missing_fields:
        print(f"  {f}")
    sys.exit(1 if missing_flags or missing_subtypes or missing_fields else 0)


if __name__ == "__main__":
    main()
