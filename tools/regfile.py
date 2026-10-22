"""Render one hive's registry config (JSON from nix/module.nix) as a .reg file.

The output is pure ASCII so the exact same bytes can be merged by hivexregedit
(the build-time check) and, re-encoded as UTF-16LE, imported by reg.exe at
install time. Anything that isn't printable ASCII goes out as hex(N), which
also sidesteps both tools' differing ideas of string escaping.

usage: regfile.py CONFIG.json ROOT OUT.reg
"""

import json
import sys


def utf16z(s: str) -> bytes:
    return s.encode("utf-16-le") + b"\x00\x00"


def hexbytes(b: bytes) -> str:
    return ",".join(f"{x:02x}" for x in b)


def is_plain(s: str) -> bool:
    return all(0x20 <= ord(c) < 0x7F for c in s)


def quote(s: str) -> str:
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def render_name(name: str) -> str:
    if name == "":
        return "@"
    if not is_plain(name):
        sys.exit(f"value name {name!r}: only printable ASCII names are supported")
    return quote(name)


def render_value(value) -> str:
    if value is None:
        return "-"
    # nix/module.nix types values as attrTag, so there is exactly one key.
    ((tag, v),) = value.items()
    match tag:
        case "sz" if is_plain(v):
            return quote(v)
        case "sz":
            return "hex(1):" + hexbytes(utf16z(v))
        case "expandSz":
            return "hex(2):" + hexbytes(utf16z(v))
        case "multiSz":
            return "hex(7):" + hexbytes(b"".join(utf16z(s) for s in v) + b"\x00\x00")
        case "dword":
            return f"dword:{v:08x}"
        case "qword":
            return "hex(b):" + hexbytes(v.to_bytes(8, "little"))
        case "binary":
            return "hex:" + hexbytes(bytes(v))
    sys.exit(f"unknown registry value type {tag!r}")


def main() -> None:
    config_path, root, out_path = sys.argv[1:]
    with open(config_path) as f:
        keys = json.load(f)

    # reg.exe creates missing parents, hivexregedit refuses to. Listing every
    # ancestor as an empty section satisfies both.
    sections = {}
    for key in keys:
        if not is_plain(key):
            sys.exit(f"key {key!r}: only printable ASCII key paths are supported")
        parts = key.split("\\")
        for i in range(1, len(parts)):
            sections.setdefault("\\".join(parts[:i]), {})
        sections[key] = keys[key]

    lines = ["Windows Registry Editor Version 5.00", ""]
    for key in sorted(sections, key=lambda k: k.lower().split("\\")):
        lines.append(f"[{root}\\{key}]")
        for name, value in sorted(sections[key].items()):
            lines.append(f"{render_name(name)}={render_value(value)}")
        lines.append("")

    with open(out_path, "w", encoding="ascii", newline="\r\n") as f:
        f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
