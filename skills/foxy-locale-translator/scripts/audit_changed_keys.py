#!/usr/bin/env python3
"""Audit a Foxy changed-key file before running locale validation."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

PLACEHOLDER_RE = re.compile(r"\{[A-Za-z_][A-Za-z0-9_]*\}")


def read_keys(path: Path) -> list[str]:
    text = path.read_text(encoding="utf-8-sig")
    keys: list[str] = []
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        keys.append(line.replace(r"\n", "\n"))
    return keys


def placeholder_set(value: object) -> set[str]:
    if isinstance(value, str):
        return set(PLACEHOLDER_RE.findall(value))
    if isinstance(value, dict):
        found: set[str] = set()
        for item in value.values():
            found.update(placeholder_set(item))
        return found
    return set()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default=".", help="Path to the Foxy repository root")
    parser.add_argument(
        "--keys",
        required=True,
        help="UTF-8 text file containing exact en.json keys, one per line; use \\n for newline characters",
    )
    args = parser.parse_args()

    repo = Path(args.repo).resolve()
    key_file = Path(args.keys).resolve()
    locale_dir = repo / "src" / "ui" / "locales"
    en_file = locale_dir / "en.json"

    if not en_file.is_file():
        raise SystemExit(f"Not a Foxy repo root or missing en.json: {repo}")
    if not key_file.is_file():
        raise SystemExit(f"Changed-key file not found: {key_file}")

    en_data = json.loads(en_file.read_text(encoding="utf-8-sig"))
    keys = read_keys(key_file)
    if not keys:
        raise SystemExit("Changed-key file is empty")

    errors: list[str] = []
    for key in keys:
        if key not in en_data:
            errors.append(f"Key is not present in en.json: {key!r}")

    locale_files = sorted(path for path in locale_dir.glob("*.json") if path.name != "en.json")
    for locale_file in locale_files:
        locale_data = json.loads(locale_file.read_text(encoding="utf-8-sig"))
        for key in keys:
            if key not in en_data or key not in locale_data:
                continue
            source_placeholders = placeholder_set(en_data[key])
            target_placeholders = placeholder_set(locale_data[key])
            if source_placeholders != target_placeholders:
                errors.append(
                    f"{locale_file.name}: placeholder mismatch for {key!r}: "
                    f"expected {sorted(source_placeholders)}, got {sorted(target_placeholders)}"
                )

    if errors:
        print("Changed-key audit failed:")
        for error in errors:
            print(f"- {error}")
        return 1

    print(f"Changed-key audit passed for {len(keys)} key(s) across {len(locale_files)} locale(s).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
