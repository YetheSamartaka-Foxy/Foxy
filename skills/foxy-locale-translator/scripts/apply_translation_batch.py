#!/usr/bin/env python3
"""Apply a targeted Foxy locale translation batch without formatter churn.

Input shape:
{
  "de": {
    "English key from en.json": "German value"
  },
  "pt-BR": {
    "English key from en.json": "Brazilian Portuguese value"
  }
}
"""

from __future__ import annotations

import argparse
import json
import re
from collections import OrderedDict
from pathlib import Path

PLACEHOLDER_RE = re.compile(r"\{[A-Za-z_][A-Za-z0-9_]*\}")


def load_ordered_json(path: Path) -> OrderedDict[str, object]:
    return json.loads(path.read_text(encoding="utf-8-sig"), object_pairs_hook=OrderedDict)


def placeholder_set(value: object) -> set[str]:
    if isinstance(value, str):
        return set(PLACEHOLDER_RE.findall(value))
    if isinstance(value, dict):
        found: set[str] = set()
        for item in value.values():
            found.update(placeholder_set(item))
        return found
    return set()


def read_batch(path: Path) -> OrderedDict[str, OrderedDict[str, object]]:
    raw = json.loads(path.read_text(encoding="utf-8-sig"), object_pairs_hook=OrderedDict)
    if not isinstance(raw, dict):
        raise SystemExit("Translation batch must be a JSON object keyed by locale code")
    batch: OrderedDict[str, OrderedDict[str, object]] = OrderedDict()
    for locale, translations in raw.items():
        if not isinstance(locale, str) or not isinstance(translations, dict):
            raise SystemExit("Translation batch entries must be locale objects")
        batch[locale] = OrderedDict(translations)
    return batch


def line_ending(lines: list[str]) -> str:
    return "\r\n" if any(line.endswith("\r\n") for line in lines) else "\n"


def serialized_key(key: str) -> str:
    return json.dumps(key, ensure_ascii=False)


def formatted_line(key: str, value: object, newline: str, comma: bool = True) -> str:
    suffix = "," if comma else ""
    return f"    {serialized_key(key)}: {json.dumps(value, ensure_ascii=False)}{suffix}{newline}"


def find_key_line(lines: list[str], key: str) -> int | None:
    needle = f"    {serialized_key(key)}:"
    for index, line in enumerate(lines):
        if line.startswith(needle):
            return index
    return None


def previous_en_key(en_keys: list[str], target: str, available: set[str]) -> str | None:
    try:
        index = en_keys.index(target)
    except ValueError:
        return None
    for prior in reversed(en_keys[:index]):
        if prior in available:
            return prior
    return None


def write_changed_key_file(path: Path, keys: list[str]) -> None:
    path.write_text("".join(key.replace("\n", r"\n") + "\n" for key in keys), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default=".", help="Path to the Foxy repository root")
    parser.add_argument("--translations", required=True, help="UTF-8 JSON translation batch")
    parser.add_argument(
        "--after-key",
        help="Insert missing keys after this key. Defaults to the closest previous en.json key present in each locale.",
    )
    parser.add_argument("--keys-out", help="Write the unique changed en.json keys to this UTF-8 file")
    parser.add_argument("--dry-run", action="store_true", help="Validate and report without writing locale files")
    parser.add_argument(
        "--allow-question-mark",
        action="store_true",
        help="Allow literal '?' in translated values after manual review",
    )
    args = parser.parse_args()

    repo = Path(args.repo).resolve()
    locale_dir = repo / "src" / "ui" / "locales"
    en_path = locale_dir / "en.json"
    if not en_path.is_file():
        raise SystemExit(f"Missing en.json under {locale_dir}")

    en_data = load_ordered_json(en_path)
    en_keys = list(en_data.keys())
    batch = read_batch(Path(args.translations).resolve())

    errors: list[str] = []
    touched_files = 0
    touched_values = 0
    changed_keys: list[str] = []

    for locale, translations in batch.items():
        locale_path = locale_dir / f"{locale}.json"
        if locale == "en":
            errors.append("Do not include en in the translation batch; en.json is the source")
            continue
        if not locale_path.is_file():
            errors.append(f"Unknown locale: {locale}")
            continue

        locale_data = load_ordered_json(locale_path)
        lines = locale_path.read_text(encoding="utf-8-sig").splitlines(keepends=True)
        newline = line_ending(lines)
        changed_here = 0

        for key, translated_value in translations.items():
            if key not in en_data:
                errors.append(f"{locale}: key is not present in en.json: {key!r}")
                continue
            if not isinstance(translated_value, (str, dict)):
                errors.append(f"{locale}: translated value must be a string or plural object for {key!r}")
                continue
            if not args.allow_question_mark and "?" in json.dumps(translated_value, ensure_ascii=False):
                errors.append(f"{locale}: translated value contains literal '?' for {key!r}")

            expected_placeholders = placeholder_set(en_data[key])
            actual_placeholders = placeholder_set(translated_value)
            if expected_placeholders != actual_placeholders:
                errors.append(
                    f"{locale}: placeholder mismatch for {key!r}: "
                    f"expected {sorted(expected_placeholders)}, got {sorted(actual_placeholders)}"
                )
                continue

            line_index = find_key_line(lines, key)
            if line_index is not None:
                comma = lines[line_index].rstrip("\r\n").endswith(",")
                new_line = formatted_line(key, translated_value, newline, comma=comma)
                if lines[line_index] != new_line:
                    lines[line_index] = new_line
                    changed_here += 1
                    if key not in changed_keys:
                        changed_keys.append(key)
                continue

            if args.after_key:
                anchor_key = args.after_key
            else:
                anchor_key = previous_en_key(en_keys, key, set(locale_data.keys()) | set(translations.keys()))
            if not anchor_key:
                errors.append(f"{locale}: could not infer insertion point for {key!r}; pass --after-key")
                continue
            insert_at = find_key_line(lines, anchor_key)
            if insert_at is None:
                errors.append(f"{locale}: insertion key not found for {key!r}: {anchor_key!r}")
                continue
            lines.insert(insert_at + 1, formatted_line(key, translated_value, newline))
            locale_data[key] = translated_value
            changed_here += 1

            if key not in changed_keys:
                changed_keys.append(key)

        if changed_here:
            touched_files += 1
            touched_values += changed_here
            if not args.dry_run:
                locale_path.write_text("".join(lines), encoding="utf-8", newline="")

    if errors:
        print("Translation batch failed:")
        for error in errors:
            print(f"- {error}")
        return 1

    if args.keys_out and not args.dry_run:
        write_changed_key_file(Path(args.keys_out).resolve(), changed_keys)

    action = "Would update" if args.dry_run else "Updated"
    print(f"{action} {touched_values} value(s) across {touched_files} locale file(s).")
    if args.keys_out:
        print(f"Changed-key file: {Path(args.keys_out).resolve()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
