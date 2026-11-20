#!/usr/bin/env python3
"""
Extract the bundled JavaScript of a Claude Code binary into readable files, so the
wire protocol, hook events and schemas can be read and cited as `chunk-*.js:line`.

Claude Code ships as a Bun single-file executable (`bun build --compile`). The binary
embeds Bun's StandaloneModuleGraph, located by its trailer:
  - trailer: <offsets struct (32 bytes)> + "\\n---- Bun! ----\\n"
  - offsets: byte_count(u64) modules_off(u32) modules_len(u32) entry_id(u32) ...
  - module list: 52-byte entries = 6 StringPointers (name, contents, sourcemap,
    bytecode, module_info, bytecode_origin_path) + 4 flag bytes
    (encoding, loader, module_format, side)
  - all offsets are relative to  trailer_pos - 32 - byte_count

Output (<out>/<version>/):
  cli.js               entry module
  chunk-*.js           every other JS module; /$bunfs/root/ imports rewritten to ./
  __assets/<name>      every non-JS embedded file (text assets transcoded to UTF-8)
  _manifest.json       binary path, version, module and asset counts

JS files are formatted with biome (fetched through `bunx`) when bun is on PATH.

Usage:
  scripts/decompile-claude.py                    # newest binary in ~/.local/share/claude/versions
  scripts/decompile-claude.py <binary> [<out>]   # default <out>: target/claude-src
"""

import json
import re
import shutil
import struct
import subprocess
import sys
from pathlib import Path

TRAILER = b"\n---- Bun! ----\n"
OFFSETS_SIZE = 32
ENTRY_SIZE = 52
BUNFS_PREFIX = "/$bunfs/root/"

LOADER_JS = 1
LOADER_NATIVE = 10
LOADER_TEXT = 13

# Bun stores text containing non-Latin-1 characters as UTF-16LE.
_TEXT_CODECS = {1: "latin-1", 2: "utf-16-le"}

_BUNFS_IMPORT = re.compile(rb'"(?:/\$bunfs/root|B:/~BUN/root)/(chunk-[a-zA-Z0-9_\-]+\.js)"')

VERSIONS_DIR = Path.home() / ".local" / "share" / "claude" / "versions"
REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUT = REPO_ROOT / "target" / "claude-src"
BIOME = "@biomejs/biome@2"
BIOME_CONFIG = {
    "formatter": {"lineWidth": 100, "indentStyle": "space"},
    "files": {"maxSize": 100_000_000, "includes": ["**/*.js", "!**/__assets"]},
    "linter": {"enabled": False},
    "assist": {"enabled": False},
}


# --- pure core -------------------------------------------------------------


def load_module_graph(data: bytes) -> tuple[list[dict], int]:
    """Return ([{name, contents, encoding, loader}], entry_point_id)."""
    trailer_pos = data.rfind(TRAILER)
    if trailer_pos < 0:
        raise SystemExit("ERROR: no Bun trailer in binary (not a `bun build --compile` executable)")

    byte_count, mod_off, mod_len, entry_id, *_ = struct.unpack(
        "<QIIIIII", data[trailer_pos - OFFSETS_SIZE : trailer_pos]
    )
    base = trailer_pos - OFFSETS_SIZE - byte_count

    def span(off: int, length: int) -> bytes:
        return data[base + off : base + off + length]

    modules = []
    for i in range(mod_len // ENTRY_SIZE):
        raw = span(mod_off + i * ENTRY_SIZE, ENTRY_SIZE)
        ptrs = struct.unpack("<12I", raw[:48])
        encoding, loader, _fmt, _side = raw[48:52]
        modules.append(
            {
                "name": span(ptrs[0], ptrs[1]).decode("utf-8", "replace"),
                "contents": span(ptrs[2], ptrs[3]),
                "encoding": encoding,
                "loader": loader,
            }
        )
    return modules, entry_id


def relative_name(module_name: str) -> str:
    if not module_name.startswith(BUNFS_PREFIX):
        raise SystemExit(f"ERROR: unexpected module path {module_name!r}")
    return module_name[len(BUNFS_PREFIX) :]


def strip_js_header(content: bytes) -> bytes:
    """Drop the `// @bun @bytecode` header and the license comment block."""
    lines = content.split(b"\n")
    js_start = next((j for j, ln in enumerate(lines) if ln and not ln.startswith(b"/")), 0)
    return b"\n".join(lines[js_start:]).rstrip(b"\x00\n")


def rewrite_chunk_imports(body: bytes) -> bytes:
    """/$bunfs/root/chunk-x.js -> ./chunk-x.js, so editors can follow imports."""
    return _BUNFS_IMPORT.sub(lambda m: b'"./' + m.group(1) + b'"', body)


def text_asset_bytes(contents: bytes, encoding: int) -> bytes:
    codec = _TEXT_CODECS.get(encoding)
    return contents if codec is None else contents.decode(codec).encode("utf-8")


def find_version(entry_js: bytes) -> str:
    m = re.search(rb'VERSION:"([^"]+)"', entry_js)
    return m.group(1).decode() if m else "unknown"


def version_key(name: str) -> tuple:
    return tuple(int(p) if p.isdigit() else -1 for p in re.split(r"[.\-]", name))


# --- imperative shell ------------------------------------------------------


def newest_installed_binary() -> Path:
    if not VERSIONS_DIR.is_dir():
        raise SystemExit(f"ERROR: {VERSIONS_DIR} not found; pass the binary path explicitly")
    candidates = [p for p in VERSIONS_DIR.iterdir() if p.is_file()]
    if not candidates:
        raise SystemExit(f"ERROR: no binaries in {VERSIONS_DIR}")
    return max(candidates, key=lambda p: version_key(p.name))


def extract(binary: Path, out_root: Path) -> Path:
    modules, entry_id = load_module_graph(binary.read_bytes())
    if not 0 <= entry_id < len(modules):
        raise SystemExit(f"ERROR: entry id {entry_id} outside module list ({len(modules)})")
    version = find_version(modules[entry_id]["contents"])
    out = out_root / version
    shutil.rmtree(out, ignore_errors=True)  # output is a pure function of the binary
    out.mkdir(parents=True)
    print(f"[extract] {binary} → {out} (version {version}, {len(modules)} modules)")

    js_count = asset_count = 0
    for i, mod in enumerate(modules):
        loader = mod["loader"]
        if loader == LOADER_NATIVE:
            continue
        rel = relative_name(mod["name"])
        if loader == LOADER_JS:
            dest = out / ("cli.js" if i == entry_id else rel)
            body = rewrite_chunk_imports(strip_js_header(mod["contents"]))
            js_count += 1
        else:
            dest = out / "__assets" / rel
            body = mod["contents"]
            if loader == LOADER_TEXT:
                body = text_asset_bytes(body, mod["encoding"])
            asset_count += 1
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(body)

    manifest = {
        "binary_path": str(binary),
        "claude_version": version,
        "js_modules": js_count,
        "assets": asset_count,
        "entry_file": "cli.js",
    }
    (out / "_manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"[extract] wrote {js_count} JS modules + {asset_count} assets")
    return out


def format_js(out: Path) -> None:
    if shutil.which("bunx") is None:
        print("[format] skipped: bunx not on PATH (files stay minified)")
        return
    config = out / "biome.json"
    config.write_text(json.dumps(BIOME_CONFIG))
    print(f"[format] {BIOME}")
    try:
        subprocess.run(
            ["bunx", BIOME, "format", "--write", "--max-diagnostics=0", "."],
            cwd=out,
            check=True,
        )
    finally:
        config.unlink()


def main(argv: list[str]) -> None:
    wants_help = any(a in ("-h", "--help") for a in argv)
    if wants_help or len(argv) > 2:
        print(__doc__)
        sys.exit(0 if wants_help else 2)
    binary = Path(argv[0]).expanduser().resolve() if argv else newest_installed_binary()
    out_root = Path(argv[1]).expanduser().resolve() if len(argv) == 2 else DEFAULT_OUT
    out = extract(binary, out_root)
    format_js(out)
    print(f"[done] {out}")


if __name__ == "__main__":
    main(sys.argv[1:])
