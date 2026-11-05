# Noumen

[中文](README.md)

Noumen is a lightweight Type-1 hypervisor for x86_64, written in Rust `no_std`. It supports both Intel VMX/EPT and AMD SVM/NPT.

The loader in this repository is a Windows kernel driver: it allocates nonpaged memory, builds `KernelArgs`, and calls the entry point. The hypervisor core only depends on the x86_64 virtualization extensions and handles CPU virtualization, EPT/NPT, hooks, logging, and memory primitives.

New agents should read [AGENTS.md](AGENTS.md) first.

[screenshot1](./imgs/s1.png) [screenshot2](./imgs/s2.png)

## Capabilities

- Intel VMX: VMCS, EPT, VPID, MSR bitmap, MTRR, EPT hooks / EPT native breakpoints / HideSelf;
- AMD SVM: VMCB, NPT, MSRPM, NPT hooks / HideSelf (NPT hooks are incomplete and there is no NPT native breakpoint);
- A unified hypercall ABI with per-backend dispatch;
- `MEM_*` batch read/write, translation, scanning, physical ranges, and statistics;
- COM1 serial output (explicit opt-in), the log ring, and detailed panic reports.

## Features

| feature | Default | Purpose |
| --- | --- | --- |
| `serial-console` | No | COM1 serial output; must be enabled explicitly |
| `buffer-log` | Yes | 512-entry log ring (about 76 KiB); loader-provided, or allocated from the page pool when `log_base == 0` |
| `panic-detail` | Yes | Panic register snapshot, `KernelArgs`, and symbol backtrace |
| `vmx-ept` | Yes | Intel backend, identity EPT, EPT hook/mem |
| `svm-npt` | Yes | AMD identity NPT, NPF, NPT hook/mem |
| `mem` | Yes | Generic `MEM_*` service; implemented by each backend |

Notes:

- `mem` no longer implies EPT. With `--no-default-features`, explicitly add `vmx-ept` or `svm-npt`.
- With `vmx-ept` disabled, VMX still runs, but EPT hook/mem code is not compiled and the related hypercalls return `RAX = 3`.
- With `svm-npt` disabled, the SVM core still compiles; only NPT/NPF/NPT hook/mem/HideSelf are disabled. The guest runs on the host page tables, and NPT-related hypercalls return `3`.
- With both `serial-console` and `buffer-log` disabled, `klog!` call sites remain (to avoid unused warnings) but produce no output.

### Windows serial output

Windows claims the serial port after boot, which makes `serial-console` ineffective. Remove `C:\WINDOWS\System32\drivers\serial.sys`.

## Build

Repository layout (the build scripts expect it):

```text
build.py                 hypervisor + both loaders (or one with --flavor)
build_deviceio.py        hypervisor + kernel/deviceio (device-IO flavor)
build_kdmapper.py        hypervisor + kernel/kdmapper (manual-mapping flavor)
hypervisor/              the Rust hypervisor crate (produces the `noumen` ELF)
loader/windows/kernel/   both kernel drivers: deviceio/, kdmapper/ (image embedded)
loader/windows/um/       both front-ends: uapp/ (device-IO), nsvr/ (kdmapper)
tools/rdtsc_probe/       RDTSC timing probe
```

Artifacts are staged under `target/<flavor>/`:

```text
target/deviceio/   noumen, noumen_loader.sys, uapp.exe
target/kdmapper/   noumen, noumen_kdmapper.sys, nsvr.exe
```

```powershell
# Both loaders (default), Release
python build.py

# Only one loader
python build_deviceio.py
python build_kdmapper.py

# Debug, skip the driver
python build.py --debug --skip-driver

# No serial console
python build.py --no-serial

# List every available feature
python build.py --list-features

# Explicitly pass the full default feature set (add serial-console for COM1)
python build.py --features panic-detail,buffer-log,vmx-ept,svm-npt,mem
```

`build_kdmapper.py` regenerates `loader/windows/kernel/kdmapper/noumen_data.h`
from the freshly built ELF before invoking MSBuild (the header is only rewritten
when the image changed).

To build only the hypervisor, cargo can be used directly from `hypervisor/`:

```powershell
# Default: logbuffer + panic detail + VMX EPT + SVM NPT + mem
cargo build
cargo build --release

# Single backend
cargo build --no-default-features --features vmx-ept
cargo build --no-default-features --features svm-npt

# Minimal image
cargo build --release --no-default-features
```

Output:

```text
hypervisor\target\x86_64-unknown-none\<profile>\noumen
```

Toolchain requirements:

- nightly Rust with `build-std`;
- Python 3.10+, standard library only, for `build*.py` and the host test scripts;
- LLVM `ld.lld.exe`, configured in `.cargo/config.toml`; the default is `C:\Program Files\LLVM\bin\ld.lld.exe`;
- clang (`build.rs` points `TMP`/`TEMP` at `OUT_DIR`);
- Visual Studio + WDK (MSBuild) for the loader drivers.

Common pitfalls:

- the current nightly does not bundle `rust-lld`; do not switch back to `rust-lld`;
- do not use `.cargo-target`; all outputs belong under the repository `target/`;
- verify every feature combination separately: default, release, `vmx-ept` only, `svm-npt` only, and no features.

## Tests

```powershell
# EPT hook pure logic
python tests/test-ept-core.py

# SVM pure logic
python tests/test-svm-core.py

# mem pure logic
python tests/test-mem-core.py
```

The test scripts live under `tests/` and are not run directly on the bare-metal target. `tests/svm-core-tests` is a standalone host harness.

Hardware paths (EPT violation, `#BP`, MTF, VMRUN, NPT TLB, NPF) must be verified on real hardware. See section 20 of [AGENTS.md](AGENTS.md).

## Hypercall

Register convention:

```text
RAX = loader key
RCX = command
RDX/R8/R9 = arguments
RAX = return value
```

- Bad key: inject `#UD`;
- Unknown command: VMX injects `#UD`, SVM returns `RAX = 3`;
- Command unsupported by the backend: return `RAX = 3`;
- Command numbers are ABI; only append, never reorder.

Shared definitions live in [`src/arch/x86_64/hypercall.rs`](src/arch/x86_64/hypercall.rs); dispatch lives in:

- [`src/arch/x86_64/vmx/hypercall/`](src/arch/x86_64/vmx/hypercall/);
- [`src/arch/x86_64/svm/hypercall/`](src/arch/x86_64/svm/hypercall/).

Command families:

- `1..=4`: logging, ping, Stop;
- `5..=7`, `9..=11`, `13..=22`, `0x100`: hooks, EPT native breakpoints, introspection; SVM has no NPT native breakpoint path;
- `0x20..=0x24`: `MEM_*`;
- `0x30..=0x3a`: SVM-only.

## Hook model

### VMX/EPT

- Guest physical pages are never written with `0xCC` or a detour;
- The fake page is execute-only; data reads go to the real page;
- The entry uses `mov r11, imm64; jmp r11` to avoid reading from an execute-only page;
- Stolen bytes are copied into the origin/trampoline, which ends with a jump back to `target + stolen`;
- The old origin is released only after all CPUs have applied the new EPT state and retired the old one;
- Real pages are restored before `STOP`.

See section 19 of [AGENTS.md](AGENTS.md) for details.

### SVM/NPT

> **Current status: incomplete.**
> NPT does not have EPT's execute-only semantics. The current implementation can only use the `OriginalExec` / `ShadowNx` / `ShadowExec` leaf views and time-slice between the data window and the execution window. This is not equivalent to EPT's full separation and must not be described as "completely invisible".
>
> SVM does not implement NPT native breakpoints: `UserBreakpoint` (kind 4) returns `Unsupported`, and the wait protocol has no corresponding breakpoint installation path.

Current implementation:

- identity NPT uses 2 MiB pages by default; hook pages are split to 4 KiB;
- data reads and instruction fetches share one leaf; after a hook hit, the data view is selected first and the execution view is selected on fetch;
- every exit restores `ShadowNx`, and a leaf switch invalidates the NPT TLB;
- all real pages are restored before `STOP`; otherwise `EFER.SVME=0` leaves the guest with `0x3B`.

Future complete implementation:

- use **dual page tables** (separate data NPT and execution NPT roots) to separate data and execution views;
- stop relying on time-slicing a single leaf, targeting EPT-level stealth; the exact switching and TLB policy will be determined during implementation.

See section 20 of [AGENTS.md](AGENTS.md) for details.

## Memory service

`MEM_*` provides:

- `MEM_QUERY`: ABI version, batch limit, maximum bytes, direct-map size;
- `MEM_BATCH`: batch read/write/translate through a guest-provided request ring;
- `MEM_SCAN`: segmented scanning;
- `MEM_MAP`: physical ranges;
- `MEM_STATS`: per-CPU statistics.

VMX uses EPT views; SVM uses identity NPT plus the direct map. Failures are reported through `RAX = MemError`; no exception is injected.

See section 21 of [AGENTS.md](AGENTS.md) for details.

## Logging

- By default, only the log ring is active; `serial-console` must be enabled explicitly for COM1;
- The ring holds 512 entries of 152 bytes each;
- The loader supplies the ring via `KernelArgs.log_base/log_size`; when `log_base == 0` (`log::init(0, _)`) the hypervisor allocates an internal ring from the page pool, so no loader log area is required, and the guest drains it with `FLUSH_LOGS`(1);
- Writers use a seqlock; readers read `sequence`, copy the record, then read it again and retry if it changed;
- Gaps in `id` indicate overwritten records;
- For no-serial images, the loader's periodic thread writes the ring to disk; crash dumps are searched for `noumen.hv.log`.

## Documentation

Long-lived documentation:

- [README.md](README.md): Chinese project overview, features, build, and logging;
- [README_en.md](README_en.md): this English overview;
- [AGENTS.md](AGENTS.md): architecture, ABI, pitfalls, and EPT/SVM/mem details.

## Known limitations

- Host tests do not cover hardware virtualization paths;
- NPT is currently incomplete: it has no execute-only support, NPT hooks rely on time-sliced views, and stealth is weaker than EPT. Dual NPT page tables are the planned complete implementation;
- In single-core mode, only the virtualized CPU can execute hypercalls.

## Acknowledgements

- [wbenny/hvpp](https://github.com/wbenny/hvpp)
- [jonomango/hv](https://github.com/jonomango/hv)
- [mikasaw/svmb](https://github.com/mikasaw/svmb)
