#!/usr/bin/env python3
"""Audit only locale values changed relative to a git baseline.

Use this after cleaning up exact-English fallbacks. Unlike a changed-key file
check, this does not require every locale for a key to differ from English; it
only validates locale/key pairs that were actually edited.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from collections import OrderedDict
from pathlib import Path

PLACEHOLDER_RE = re.compile(r"\{[A-Za-z_][A-Za-z0-9_]*\}")


def load_json(path: Path) -> OrderedDict[str, object]:
    return json.loads(path.read_text(encoding="utf-8-sig"), object_pairs_hook=OrderedDict)


def git_show(repo: Path, ref: str, rel_path: str) -> OrderedDict[str, object] | None:
    result = subprocess.run(
        ["git", "show", f"{ref}:{rel_path}"],
        cwd=repo,
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if result.returncode != 0:
        return None
    return json.loads(result.stdout.lstrip("\ufeff"), object_pairs_hook=OrderedDict)


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
    parser.add_argument("--baseline", default="HEAD", help="Git ref to compare against")
    parser.add_argument(
        "--allow-key-file",
        help="Optional UTF-8 file of en.json keys allowed to remain exact-English when changed",
    )
    args = parser.parse_args()

    repo = Path(args.repo).resolve()
    locale_dir = repo / "src" / "ui" / "locales"
    en_path = locale_dir / "en.json"
    if not en_path.is_file():
        raise SystemExit(f"Missing en.json under {locale_dir}")

    allowed: set[str] = set()
    if args.allow_key_file:
        for raw_line in Path(args.allow_key_file).read_text(encoding="utf-8-sig").splitlines():
            line = raw_line.strip()
            if line and not line.startswith("#"):
                allowed.add(line.replace(r"\n", "\n"))

    en_data = load_json(en_path)
    errors: list[str] = []
    changed_files = 0
    changed_pairs = 0
    changed_keys: set[str] = set()

    for locale_path in sorted(path for path in locale_dir.glob("*.json") if path.name != "en.json"):
        rel_path = locale_path.relative_to(repo).as_posix()
        baseline_data = git_show(repo, args.baseline, rel_path)
        if baseline_data is None:
            continue

        locale_data = load_json(locale_path)
        local_changes = 0
        for key, value in locale_data.items():
            if key not in baseline_data or baseline_data[key] == value:
                continue

            local_changes += 1
            changed_pairs += 1
            changed_keys.add(key)

            expected_placeholders = placeholder_set(en_data.get(key))
            actual_placeholders = placeholder_set(value)
            if expected_placeholders != actual_placeholders:
                errors.append(
                    f"{locale_path.name}: placeholder mismatch for {key!r}: "
                    f"expected {sorted(expected_placeholders)}, got {sorted(actual_placeholders)}"
                )

            if key not in allowed and key in en_data and value == en_data[key]:
                errors.append(f"{locale_path.name}: changed value still equals en.json for {key!r}")

        if local_changes:
            changed_files += 1

    print(
        f"Audited {changed_pairs} changed locale value(s) "
        f"across {changed_files} file(s), {len(changed_keys)} unique key(s)."
    )

    if errors:
        print("Changed locale pair audit failed:")
        for error in errors:
            print(f"- {error}")
        return 1

    print("Changed locale pair audit passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
