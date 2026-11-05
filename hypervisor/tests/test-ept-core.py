#!/usr/bin/env python3
"""Host-side tests for the EPT hook core logic.

The hypervisor crate targets x86_64-unknown-none and cannot use `cargo test`, so
the pure modules are compiled into a standalone Rust test crate:

  state.rs   - hook entries, PTE derivation, table lookup, generation rules
  detour.rs  - stub/trampoline/absolute jump bytes (uses ldisasm.rs)
  ldisasm.rs - instruction length decoder
  wait.rs    - EPT native breakpoint wait state machine (INT3 spin wait)

No hardware, driver or virtualization is involved.
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Run the EPT hook host-side tests.")
    parser.add_argument(
        "--output-directory",
        default="target/ept-core-tests",
        help="directory for the generated harness and test executable "
        "(default: target/ept-core-tests)",
    )
    args = parser.parse_args(argv)

    output = Path(args.output_directory)
    if not output.is_absolute():
        output = REPO_ROOT / output
    output.mkdir(parents=True, exist_ok=True)

    source_dir = REPO_ROOT / "src" / "arch" / "x86_64" / "vmx" / "ept"
    arch_dir = REPO_ROOT / "src" / "arch" / "x86_64"
    hook_dir = source_dir / "hook"
    harness = output / "harness.rs"

    lines = [
        "#![allow(dead_code)]",
        "",
        f'#[path = r"{ (arch_dir / "ldisasm.rs").as_posix() }"]',
        "mod ldisasm;",
        "",
        "mod arch {",
        "    pub mod x86_64 {",
        "        pub(crate) use crate::ldisasm;",
        "    }",
        "}",
        "",
        f'#[path = r"{ (hook_dir / "detour.rs").as_posix() }"]',
        "mod detour;",
        "",
        f'#[path = r"{ (source_dir / "state.rs").as_posix() }"]',
        "mod state;",
        "",
        f'#[path = r"{ (hook_dir / "user_breakpoint" / "wait.rs").as_posix() }"]',
        "mod wait;",
        "",
    ]
    harness.write_text("\n".join(lines), encoding="utf-8")

    executable = output / "ept_core_tests.exe"
    rustc = shutil.which("rustc")
    if rustc is None:
        print("rustc was not found in PATH", file=sys.stderr)
        return 1

    try:
        compiled = subprocess.run(
            [rustc, "--edition", "2024", "--test", str(harness), "-o", str(executable)],
            cwd=str(output),
            check=False,
        )
        if compiled.returncode != 0:
            return compiled.returncode

        tested = subprocess.run([str(executable)], cwd=str(output), check=False)
        return tested.returncode
    except OSError as error:
        print(f"failed to run the EPT core tests: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
