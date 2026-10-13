"""Writes docs/THIRD-PARTY-NOTICES.txt from the license files of every crate that is
compiled into KilatX.exe (normal dependencies for Windows x64/ARM64; build-only and
test-only crates are left out because they are not shipped).

Usage (from app/):  python testkit/third_party_notices.py
"""
import hashlib
import json
import os
import subprocess
import sys

TARGETS = ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]
LICENSE_PREFIXES = ("license", "licence", "copying", "notice", "unlicense")


def metadata(target):
    out = subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", target],
        cwd=os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    )
    return json.loads(out)


def shipped(meta):
    """Package ids reachable from the root through normal (non-build, non-dev) edges."""
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = meta["resolve"]["root"]
    seen, todo = set(), [root]
    while todo:
        pid = todo.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                todo.append(dep["pkg"])
    seen.discard(root)
    return seen


def main():
    packages = {}
    for target in TARGETS:
        meta = metadata(target)
        by_id = {p["id"]: p for p in meta["packages"]}
        for pid in shipped(meta):
            p = by_id[pid]
            packages[(p["name"], p["version"])] = p

    texts = {}  # hash -> (text, [crates])
    summary = []
    for (name, version), p in sorted(packages.items()):
        folder = os.path.dirname(p["manifest_path"])
        files = sorted(f for f in os.listdir(folder) if f.lower().startswith(LICENSE_PREFIXES) and os.path.isfile(os.path.join(folder, f)))
        summary.append(f"{name} {version}  —  {p.get('license') or p.get('license_file') or 'see files'}"
                       + (f"  —  {p['repository']}" if p.get("repository") else ""))
        for f in files:
            with open(os.path.join(folder, f), encoding="utf-8", errors="replace") as fh:
                text = fh.read().strip()
            key = hashlib.sha256(text.encode()).hexdigest()
            texts.setdefault(key, (f, text, []))[2].append(f"{name} {version}")
        if not files:
            texts.setdefault(f"none-{name}", ("(no license file in the package)", f"License: {p.get('license')}", []))[2].append(f"{name} {version}")

    out = ["THIRD-PARTY NOTICES", "", "KilatX is free software under the GNU General Public License v3 (LICENSE).",
           "It includes the following open-source components. The user interface library Slint",
           "is used under its GPL-3.0 license option.", "",
           f"Components ({len(summary)}):", ""] + summary + ["", "=" * 78, "LICENSE TEXTS", "=" * 78]
    for _, (fname, text, crates) in sorted(texts.items(), key=lambda kv: kv[1][2][0]):
        out += ["", "-" * 78, "Used by: " + ", ".join(sorted(crates)), f"({fname})", "-" * 78, "", text]
    dest = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "docs", "THIRD-PARTY-NOTICES.txt")
    with open(dest, "w", encoding="utf-8") as fh:
        fh.write("\n".join(out) + "\n")
    print(f"{len(summary)} components, {len(texts)} distinct license texts -> {dest}")


if __name__ == "__main__":
    sys.exit(main())
