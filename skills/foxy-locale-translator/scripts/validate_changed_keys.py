#!/usr/bin/env python3
"""Run Foxy's i18n checker for a file of changed locale keys."""

from __future__ import annotations

import argparse
import subprocess
from pathlib import Path


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
    if not (repo / "tools/i18n-checker/Cargo.toml").is_file():
        raise SystemExit(f"Not a Foxy repo root: {repo}")
    if not key_file.is_file():
        raise SystemExit(f"Changed-key file not found: {key_file}")

    command = [
        "cargo",
        "run",
        "--manifest-path",
        str(repo / "tools/i18n-checker/Cargo.toml"),
        "--",
        "--strict",
        "--require-translated-key-file",
        str(key_file),
    ]
    return subprocess.run(command, cwd=repo, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
