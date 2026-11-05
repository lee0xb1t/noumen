#!/usr/bin/env python3
"""生成 uapp 的 Windows 内核结构偏移表（`win_offsets_table.rs`）。

两个数据来源：

1. **memflow 的 offsets/*.toml**（`--memflow <dir>`）：直接读 `[offsets]` 里的字段，
   只取 X64 的条目（uapp 是 x64，读的也是 x64 内核）。
2. **ntoskrnl.exe 本体**（`--ntoskrnl <exe>`）：没有现成表的新 build（例如 26100）
   就地从 PE 导出表 + 几条 getter 的反汇编里取偏移。

从二进制能可靠取到的字段（都有导出 getter）：

    eproc_pid          PsGetProcessId                     mov [rcx+disp]
    eproc_name         PsGetProcessImageFileName          lea [rcx+disp]
    eproc_peb          PsGetProcessPeb                    mov [rcx+disp]
    eproc_section_base PsGetProcessSectionBaseAddress     mov [rcx+disp]
    eproc_exit_status  PsGetProcessExitStatus             mov [rcx+disp]
    eproc_wow64        PsGetProcessWow64Process           mov [rcx+disp]
    kthread_process    PsGetCurrentProcess / PsGetThreadProcess

取不到的用已知不变量补齐（括号里是验证过的 build）：

    list_blink   = 8            LIST_ENTRY 布局（全部）
    eproc_link   = eproc_pid+8  ActiveProcessLinks 紧跟 UniqueProcessId（7601/14393/18362/19041/19045）
    kproc_dtb    = 0x28         x64 _KPROCESS.DirectoryTableBase（7601/14393/18362/19041/19045）

`eproc_thread_list` / `eproc_vad_root` 目前没有可靠来源，且 uapp 没用到，写 0。

用法：

    python tools/gen_win_offsets.py --memflow <dir> --ntoskrnl <exe> [--write]
"""

from __future__ import annotations

import argparse
import re
import struct
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
TABLES = [
    REPO_ROOT / "loader" / "windows" / "deviceio" / "uapp" / "src" / "win_offsets_table.rs",
    REPO_ROOT / "loader" / "windows" / "kdmapper" / "app" / "src" / "win_offsets_table.rs",
]

#: 字段名（与 `WinOffsets` 一致）→ 输出顺序。
FIELDS = [
    "list_blink",
    "eproc_link",
    "kproc_dtb",
    "eproc_pid",
    "eproc_name",
    "eproc_peb",
    "eproc_section_base",
    "eproc_exit_status",
    "eproc_thread_list",
    "eproc_wow64",
    "eproc_vad_root",
    "kthread_process",
]

#: 手工确认过、memflow 里没有的条目（数值来源写在注释里）。
MANUAL: dict[int, dict[str, int]] = {
    # 14393：从 102426-7796-01.dmp 里 `dt nt!_EPROCESS` 直接读出来的。
    14393: {
        "list_blink": 8,
        "eproc_link": 0x2F0,
        "kproc_dtb": 0x28,
        "eproc_pid": 0x2E8,
        "eproc_name": 0x450,
        "eproc_peb": 0x3F8,
        "eproc_section_base": 0x3C0,
        "eproc_exit_status": 0x61C,
        "eproc_thread_list": 0x488,
        "eproc_wow64": 0x428,
        "eproc_vad_root": 0x620,
        "kthread_process": 0xB8,
    },
}

#: `KTHREAD.ApcState.Process` 只随大版本变：Win7 x64 是 0x68，Win8 之后是 0xB8。
KTHREAD_PROCESS_BY_BUILD = {
    3790: 0,
    7601: 0x68,
}

#: `KPCR.Prcb.CurrentThread`：x64 上从 Win7 到 24H2 都是 0x188。
#: hypervisor 里 `selective_tsc::KPCR_CURRENT_THREAD` 必须和它一致（脚本会核对）。
KPCR_CURRENT_THREAD = 0x188


def kthread_process_for(build: int) -> int:
    if build in KTHREAD_PROCESS_BY_BUILD:
        return KTHREAD_PROCESS_BY_BUILD[build]
    return 0xB8 if build >= 9200 else 0


# --------------------------------------------------------------------- PE / 反汇编


class PeImage:
    def __init__(self, path: Path) -> None:
        self.data = path.read_bytes()
        data = self.data

        e_lfanew = struct.unpack_from("<I", data, 0x3C)[0]
        if data[e_lfanew : e_lfanew + 4] != b"PE\0\0":
            raise SystemExit(f"{path}: not a PE file")

        coff = e_lfanew + 4
        _, nsec, _, _, _, opt_size, _ = struct.unpack_from("<HHIIIHH", data, coff)
        opt = coff + 20
        magic = struct.unpack_from("<H", data, opt)[0]
        dd = opt + (112 if magic == 0x20B else 96)
        self.export_rva = struct.unpack_from("<I", data, dd)[0]

        self.sections = []
        sec_off = opt + opt_size
        for i in range(nsec):
            name = data[sec_off + i * 40 : sec_off + i * 40 + 8].rstrip(b"\0").decode(errors="replace")
            vsize, vaddr, rawsize, rawptr = struct.unpack_from("<IIII", data, sec_off + i * 40 + 8)
            self.sections.append((name, vaddr, max(vsize, rawsize), rawptr))

        self.exports = self._parse_exports()

    def _rva_to_off(self, rva: int) -> int | None:
        for _, vaddr, size, rawptr in self.sections:
            if vaddr <= rva < vaddr + size:
                return rawptr + (rva - vaddr)
        return None

    def read(self, rva: int, length: int) -> bytes:
        off = self._rva_to_off(rva)
        if off is None:
            return b""
        return self.data[off : off + length]

    def u32(self, rva: int) -> int:
        return struct.unpack_from("<I", self.read(rva, 4))[0]

    def u16(self, rva: int) -> int:
        return struct.unpack_from("<H", self.read(rva, 2))[0]

    def _parse_exports(self) -> dict[str, int]:
        base = self.export_rva
        n_names = self.u32(base + 24)
        names_rva = self.u32(base + 32)
        funcs_rva = self.u32(base + 28)
        ords_rva = self.u32(base + 36)

        exports = {}
        for i in range(n_names):
            name_rva = self.u32(names_rva + i * 4)
            name = self.read(name_rva, 128).split(b"\0")[0].decode(errors="replace")
            ordinal = self.u16(ords_rva + i * 2)
            exports[name] = self.u32(funcs_rva + ordinal * 4)

        return exports


def _field_from_rcx(code: bytes) -> int | None:
    """在函数开头找 `op reg, [rcx+disp]`，返回 disp。"""
    index = 0
    while index < len(code) - 8:
        byte = code[index]

        if byte in (0x48, 0x4C, 0x65):  # 段/REX 前缀
            index += 1
            continue

        if byte in (0x8B, 0x8D, 0x89, 0x3B, 0x8A, 0x8C):
            modrm = code[index + 1]
            mod = modrm >> 6
            rm = modrm & 7

            if rm == 1:  # [rcx+disp]
                if mod == 1:
                    return code[index + 2]
                if mod == 2:
                    return struct.unpack_from("<i", code, index + 2)[0]

            index += 2
            continue

        index += 1

    return None


def _kthread_process(code: bytes) -> tuple[int | None, int | None]:
    """`PsGetCurrentProcess` 类函数：gs:[disp] 取到 KTHREAD，再 `[rax+disp]` 取 EPROCESS。"""
    marker = b"\x65\x48\x8b\x04\x25"  # mov rax, qword ptr gs:[disp32]
    at = code.find(marker)
    if at < 0:
        return (None, None)

    kpcr_offset = struct.unpack_from("<i", code, at + 5)[0]

    # 之后应出现 `48 8B 80 disp32`（mov rax,[rax+disp]）或 `48 8B 40 disp8`。
    tail = code[at + 9 :]
    for index in range(0, min(len(tail) - 6, 40)):
        if tail[index : index + 2] == b"\x48\x8b":
            modrm = tail[index + 2]
            mod, rm = modrm >> 6, modrm & 7
            if rm == 0:  # [rax+disp]
                if mod == 2:
                    return (kpcr_offset, struct.unpack_from("<i", tail, index + 3)[0])
                if mod == 1:
                    return (kpcr_offset, tail[index + 3])

    return (kpcr_offset, None)


def derive_from_ntoskrnl(path: Path) -> dict[str, int]:
    image = PeImage(path)
    exports = image.exports

    wanted = {
        "eproc_pid": "PsGetProcessId",
        "eproc_name": "PsGetProcessImageFileName",
        "eproc_peb": "PsGetProcessPeb",
        "eproc_section_base": "PsGetProcessSectionBaseAddress",
        "eproc_exit_status": "PsGetProcessExitStatus",
        "eproc_wow64": "PsGetProcessWow64Process",
    }

    found: dict[str, int] = {}
    for field, export in wanted.items():
        rva = exports.get(export)
        if rva is None:
            print(f"  ! {export} is not exported, skipping {field}", file=sys.stderr)
            continue
        code = image.read(rva, 48)
        disp = _field_from_rcx(code)
        if disp is None:
            print(f"  ! no [rcx+disp] found in {export}, skipping {field}", file=sys.stderr)
            continue
        found[field] = disp

    # KTHREAD.ApcState.Process：优先 PsGetCurrentProcess，退路 PsGetThreadProcess。
    # 注意：PsGetCurrentProcess 是 `gs:[kpcr] → KTHREAD`，再从 KTHREAD 取 EPROCESS，
    # 两级解引用；hypervisor 侧必须按同样的链读，这里顺带核对 kpcr 偏移。
    for export in ("PsGetCurrentProcess", "PsGetThreadProcess"):
        rva = exports.get(export)
        if rva is None:
            continue
        code = image.read(rva, 64)

        if export == "PsGetThreadProcess":
            disp = _field_from_rcx(code)
            if disp is not None:
                found["kthread_process"] = disp
                print(f"  kthread_process <- {export}")
                break
            continue

        kpcr_offset, process_disp = _kthread_process(code)
        if kpcr_offset is not None:
            print(f"  kpcr.current_thread = 0x{kpcr_offset:X}")
            if kpcr_offset != KPCR_CURRENT_THREAD:
                print(
                    f"  ! KPCR.CurrentThread 变成了 0x{kpcr_offset:X}，"
                    f"hypervisor 里的 KPCR_CURRENT_THREAD 常量要跟着改",
                    file=sys.stderr,
                )
        if process_disp is not None:
            found["kthread_process"] = process_disp
            print(f"  kthread_process <- {export}")
            break

    # 不变量补齐。
    if "eproc_pid" in found:
        found.setdefault("list_blink", 8)
        found.setdefault("eproc_link", found["eproc_pid"] + 8)
        found.setdefault("kproc_dtb", 0x28)
        found.setdefault("kthread_process", 0xB8)

    found.setdefault("eproc_thread_list", 0)
    found.setdefault("eproc_vad_root", 0)
    found.setdefault("list_blink", 8)
    found.setdefault("kproc_dtb", 0x28)

    return found


# ------------------------------------------------------------------------- 数据源


def ntoskrnl_build(path: Path) -> int:
    """从 PE 版本资源里取 build 号。

    不解析整棵 VS_VERSIONINFO：直接找 `VS_FIXEDFILEINFO` 的签名（0xFEEF04BD），
    紧跟其后的结构里 `dwFileVersionLS` 的高 16 位就是 build 号。
    """
    data = path.read_bytes()
    signature = struct.pack("<I", 0xFEEF04BD)
    at = data.find(signature)
    if at < 0:
        raise SystemExit(f"{path}: no VS_FIXEDFILEINFO signature")

    _, _, version_ms, version_ls = struct.unpack_from("<IIII", data, at)
    major, minor = version_ms >> 16, version_ms & 0xFFFF
    build, revision = version_ls >> 16, version_ls & 0xFFFF

    print(f"  version {major}.{minor}.{build}.{revision}")

    return build


def load_memflow(directory: Path) -> dict[int, dict[str, int]]:
    entries: dict[int, dict[str, int]] = {}

    for toml in sorted(directory.glob("*.toml")):
        text = toml.read_text(encoding="utf-8", errors="replace")
        if "arch = 'X64'" not in text and 'arch = "X64"' not in text:
            continue

        values: dict[str, int] = {}
        header: dict[str, str] = {}
        section = ""

        for raw in text.splitlines():
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            if line.startswith("["):
                section = line.strip("[]")
                continue
            if "=" not in line:
                continue

            key, _, value = line.partition("=")
            key = key.strip()
            value = value.strip().strip("'\"")

            if section == "header":
                header[key] = value
            elif section == "offsets":
                try:
                    values[key] = int(value, 0)
                except ValueError:
                    continue

        if "nt_build_number" not in header:
            continue

        build = int(header["nt_build_number"])
        entry = dict(MANUAL.get(build, {}))

        for field in FIELDS:
            if field in values:
                entry[field] = values[field]

        entry.setdefault("list_blink", 8)
        entry.setdefault("kproc_dtb", 0x28)
        if "eproc_pid" in entry:
            entry.setdefault("eproc_link", entry["eproc_pid"] + 8)
        entry.setdefault("kthread_process", kthread_process_for(build))
        entry.setdefault("eproc_thread_list", 0)
        entry.setdefault("eproc_vad_root", 0)

        # 同一个 build 可能有多个 PDB 变体（不同补丁），取第一个。
        entries.setdefault(build, entry)

    return entries


# ------------------------------------------------------------------------ 输出


def render(entries: dict[int, dict[str, int]]) -> str:
    lines = [
        "// 由 `tools/gen_win_offsets.py` 生成，不要手改。",
        "//",
        "// 数据来源：memflow 的 offsets/*.toml（X64）+ ntoskrnl.exe 导出的 getter 反汇编。",
        "// `kthread_process` 取不到时按 KTHREAD 布局常量补（Win7 0x68，Win8+ 0xB8）。",
        "",
        f"pub const TABLE: [WinOffsets; {len(entries)}] = [",
    ]

    for build in sorted(entries):
        entry = entries[build]
        lines.append(f"    // {build}")
        lines.append("    WinOffsets {")
        lines.append(f"        build: {build},")
        for field in FIELDS:
            lines.append(f"        {field}: {entry.get(field, 0)},")
        lines.append("    },")

    lines.append("];")
    lines.append("")

    return "\n".join(lines)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--memflow", type=Path, help="memflow-win32 offsets directory")
    parser.add_argument(
        "--ntoskrnl",
        type=Path,
        action="append",
        default=[],
        help="extra ntoskrnl.exe to derive offsets from (repeatable)",
    )
    parser.add_argument("--write", action="store_true", help="write uapp win_offsets_table.rs")
    parser.add_argument("--dump", action="store_true", help="only print the result")
    args = parser.parse_args(argv)

    entries: dict[int, dict[str, int]] = {}

    if args.memflow:
        entries.update(load_memflow(args.memflow))
        print(f"memflow: {len(entries)} X64 entries")

    for build, entry in MANUAL.items():
        entries.setdefault(build, dict(entry))
        entries[build].setdefault("kthread_process", kthread_process_for(build))

    for exe in args.ntoskrnl:
        build = ntoskrnl_build(exe)
        print(f"\n{exe.name}: build {build}, deriving offsets from exports")
        derived = derive_from_ntoskrnl(exe)

        # 已有条目就**合并**：二进制能确定的字段以二进制为准（并核对它与已有值），
        # 它确定不了的（eproc_thread_list / eproc_vad_root）保留原值，别用 0 覆盖。
        existing = entries.get(build)
        if existing is None:
            entries[build] = derived
        else:
            for field in FIELDS:
                value = derived.get(field, 0)

                if value == 0:
                    continue

                old = existing.get(field)
                if old is not None and old != value:
                    print(
                        f"  * {field}: 表里是 0x{old:X}，二进制算出来 0x{value:X}，用二进制值",
                        file=sys.stderr,
                    )

                existing[field] = value

            derived = existing

        for field in FIELDS:
            print(f"  {field:20s} = 0x{derived.get(field, 0):X}  ({derived.get(field, 0)})")

    if not entries:
        print("no data source given", file=sys.stderr)
        return 1

    text = render(entries)

    if args.dump or not args.write:
        print()
        print(text)

    if args.write:
        for path in TABLES:
            path.write_text(text, encoding="utf-8", newline="\n")
            print(f"wrote {path.relative_to(REPO_ROOT)}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
