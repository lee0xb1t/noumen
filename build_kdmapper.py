#!/usr/bin/env python3
"""构建手动映射版 loader：hypervisor + loader/windows/kdmapper + uapp。

这一版的驱动把 hypervisor 镜像编进 `noumen_data.h`，所以构建前会先把编译出的
ELF 重新生成成 C 数组头文件；驱动编译结束后恢复原来的 `noumen_data.h`
（成功、失败或中断都会恢复）。

产物落到 `target/kdmapper/`：

    noumen                  hypervisor ELF（留档/对比用）
    noumen_kdmapper.sys     内嵌镜像的驱动（vcxproj 里叫 driver.sys，暂存时改名）
    uapp.exe                控制面板

等价于 `python build.py --flavor kdmapper`；额外参数原样透传。
"""

from __future__ import annotations

import sys

import build


def main(argv: list[str]) -> int:
    return build.main(["--flavor", "kdmapper", *argv])


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except build.BuildError as error:
        print(file=sys.stderr)
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1) from error
