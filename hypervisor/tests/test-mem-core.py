#!/usr/bin/env python3
"""Host-side tests for the pure mem module logic.

The hypervisor crate targets x86_64-unknown-none and cannot use `cargo test`.
The only hardware-independent part of `mem` is signature matching
(`pattern.rs`), which only depends on `core` and can be compiled as a standalone
test crate.

Real guest access requires VT-x and is outside the host test scope.
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Run the mem pattern host-side tests.")
    parser.add_argument(
        "--output-directory",
        default="target/mem-core-tests",
        help="directory for the test executable (default: target/mem-core-tests)",
    )
    args = parser.parse_args(argv)

    output = Path(args.output_directory)
    if not output.is_absolute():
        output = REPO_ROOT / output
    output.mkdir(parents=True, exist_ok=True)

    source = REPO_ROOT / "src" / "arch" / "x86_64" / "vmx" / "ept" / "mem" / "pattern.rs"
    executable = output / "pattern_tests.exe"

    rustc = shutil.which("rustc")
    if rustc is None:
        print("rustc was not found in PATH", file=sys.stderr)
        return 1

    try:
        compiled = subprocess.run(
            [rustc, "--edition", "2024", "--test", str(source), "-o", str(executable)],
            cwd=str(output),
            check=False,
        )
        if compiled.returncode != 0:
            return compiled.returncode

        tested = subprocess.run([str(executable)], cwd=str(output), check=False)
        return tested.returncode
    except OSError as error:
        print(f"failed to run the mem pattern tests: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
