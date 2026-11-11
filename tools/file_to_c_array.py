#!/usr/bin/env python3
"""Convert a binary file to a C array header.

The generated text matches the kdmapper ``noumen_data.h`` layout:

    // array size is N
    static const unsigned char noumen_data[] = {
      0x..., ...
    };

Usage:
    python tools/file_to_c_array.py input.bin -o output.h
    python tools/file_to_c_array.py input.bin -n my_data
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def format_c_array(
    data: bytes,
    name: str = "noumen_data",
    *,
    columns: int = 16,
) -> str:
    """Return ``data`` as a C array declaration with a size comment."""
    if columns <= 0:
        raise ValueError("columns must be positive")
    if not name.isidentifier():
        raise ValueError(f"invalid C identifier: {name!r}")

    lines = [
        f"// array size is {len(data)}",
        f"static const unsigned char {name}[] = {{",
    ]

    for offset in range(0, len(data), columns):
        chunk = data[offset : offset + columns]
        body = ", ".join(f"0x{byte:02x}" for byte in chunk)
        separator = "," if offset + columns < len(data) else ""
        lines.append(f"  {body}{separator}")

    lines.append("};")
    return "\n".join(lines) + "\n"


def write_c_array(
    source: Path,
    destination: Path,
    name: str = "noumen_data",
    *,
    columns: int = 16,
) -> None:
    destination.write_text(
        format_c_array(source.read_bytes(), name, columns=columns),
        encoding="utf-8",
        newline="\n",
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, help="input binary file")
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        help="output header path; omit to write to stdout",
    )
    parser.add_argument(
        "-n",
        "--name",
        default="noumen_data",
        help="C array name (default: noumen_data)",
    )
    parser.add_argument(
        "--columns",
        type=int,
        default=16,
        help="bytes per output line (default: 16)",
    )
    args = parser.parse_args(argv)

    try:
        data = args.input.read_bytes()
        text = format_c_array(data, args.name, columns=args.columns)
    except (OSError, ValueError) as error:
        parser.error(str(error))

    if args.output is None:
        sys.stdout.write(text)
    else:
        args.output.write_text(text, encoding="utf-8", newline="\n")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
