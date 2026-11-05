#!/usr/bin/env python3
"""构建设备 IO 版 loader：hypervisor + loader/windows/deviceio + uapp。

产物落到 `target/deviceio/`：

    noumen              hypervisor ELF（驱动运行时从文件加载）
    noumen_loader.sys   设备 IO 驱动
    uapp.exe            控制面板

等价于 `python build.py --flavor deviceio`；额外参数（`--debug`、`--no-serial`、
`--features`、`--skip-*` 等）原样透传，见 `python build.py --help`。
"""

from __future__ import annotations

import sys

import build


def main(argv: list[str]) -> int:
    return build.main(["--flavor", "deviceio", *argv])


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except build.BuildError as error:
        print(file=sys.stderr)
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1) from error
