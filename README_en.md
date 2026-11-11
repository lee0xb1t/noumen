# Noumen

[中文](README.md)

Noumen is a lightweight Type-1 style hypervisor for x86_64, written in Rust `no_std`. It supports both Intel VMX/EPT and AMD SVM/NPT.

The loader in this repository is a Windows kernel driver. It allocates nonpaged memory, builds `KernelArgs`, and calls the entry point; the hypervisor core only depends on the x86_64 virtualization extensions and handles CPU virtualization, EPT/NPT, hooks, logging, and memory primitives.

[screenshot1](./imgs/s1.png) [screenshot2](./imgs/s2.png)

## Before use

Noumen needs exclusive VT-x/AMD-V. Disable VBS, Memory Integrity (HVCI), Hyper-V,
WSL2, Windows Sandbox and similar virtualization features; enable nested
virtualization when running inside a VM. Disable Memory Integrity in Windows
Security, turn off those features, then reboot and confirm VBS is off with
`msinfo32`.

## Capabilities

- Intel VMX: VMCS, EPT, VPID, MSR bitmap, MTRR, EPT hooks, EPT native breakpoints, HideSelf;
- AMD SVM: VMCB, NPT, MSRPM, NPT hooks, HideSelf; NPT hooks are incomplete and there are no NPT native breakpoints;
- VMX CPUID is passed through natively; `hypervisor-msr` separately controls outer-hypervisor MSRs;
- Unified hypercall ABI with per-backend dispatch;
- `MEM_*` batch read/write, translation, scanning, physical ranges, and statistics;
- Log ring, panic details, and optional COM1 serial output.

## Features

| feature | Default | Purpose |
| --- | --- | --- |
| `serial-console` | No | COM1 serial output; explicit opt-in |
| `buffer-log` | Yes | 512-entry log ring; loader-provided or allocated from the page pool |
| `panic-detail` | Yes | Panic register snapshot, `KernelArgs`, and symbol backtrace |
| `vmx-ept` | Yes | Intel backend, identity EPT, EPT hook/mem |
| `native-bp` | No | VMX native breakpoints, #BP/#CP/NMI exits; enable explicitly when needed, depends on `vmx-ept` |
| `hypervisor-msr` | No | Pass through outer `0x40000000..=0x400000FF` MSRs; default is `#GP`, enable only for VMware/nested runs |
| `svm-npt` | Yes | AMD identity NPT, NPF, NPT hook/mem |
| `mem` | Yes | Generic `MEM_*` service, implemented per backend |

Notes:

- `mem` does not imply `vmx-ept` or `svm-npt`; add the backend feature explicitly with `--no-default-features`.
- With a backend feature disabled, the core still compiles and related hook/mem hypercalls return `RAX = 3`; VMX EPT hook/HideSelf are not compiled, and `enable_vpid`/`unrestricted_guest` are off.
- `native-bp` is off by default; enable it explicitly (it depends on `vmx-ept`) when `UserBreakpoint`/`#BP`/`#CP`/NMI are needed. Leave it off for MEM_*/HideSelf-only builds to avoid the associated TSC inflation.
- `hypervisor-msr` is off by default. VMX CPUID is always raw-passthrough; this feature only controls `0x40000000..=0x400000FF` outer-hypervisor MSRs. The default injects `#GP`; enable it only for VMware/nested environments.
- With both `serial-console` and `buffer-log` disabled, `klog!` still type-checks its arguments but produces no output.

When using `serial-console`, Windows owns COM1; the solution is to delete `C:\WINDOWS\System32\drivers\serial.sys`.

## Build

Requirements: nightly Rust with `build-std`, Python 3.10+, LLVM `ld.lld.exe`, clang, and Visual Studio + WDK.

```powershell
# Both loader flavors, Release
python build.py

# One flavor only
python build_deviceio.py
python build_kdmapper.py
python build_kdmapper.py --hypervisor-msr   # VMware/nested environments

# The kdmapper build rewrites noumen_data.h temporarily, then restores it
# Debug, skip the driver
python build.py --debug --skip-driver

# Remove intermediate outputs
python build.py --clean-only
```

Artifacts:

```text
target/deviceio/   noumen, noumen_loader.sys, uapp.exe
target/kdmapper/   noumen, noumen_kdmapper.sys, nsvr.exe, mcp-server.exe, test_mcp_server.exe
```

Hypervisor only:

```powershell
cd hypervisor
cargo build
cargo build --release
cargo build --no-default-features --features vmx-ept
cargo build --no-default-features --features svm-npt
cargo build --release --no-default-features
```

For MEM_*/HideSelf-only builds without native breakpoints:

```powershell
cargo build --release --no-default-features --features buffer-log,vmx-ept,mem
```

The image is written to `hypervisor/target/x86_64-unknown-none/<profile>/noumen`. The toolchain is pinned by `rust-toolchain.toml`; update the `ld.lld.exe` path in `.cargo/config.toml` for other machines.

## MCP hypercall server

The kdmapper build also produces a hypercall-only MCP/REST server:

```powershell
.\target\kdmapper\mcp-server.exe
```

Build, REST endpoints, MCP tools, arguments, and safety boundaries are documented
in [MCP.md](MCP.md).

## Tests

```powershell
cd hypervisor
python tests/test-ept-core.py
python tests/test-svm-core.py
python tests/test-mem-core.py
```

Host tests do not cover real hardware paths; EPT violation, `#BP`, MTF, VMRUN, NPT TLB, and NPF must be verified on hardware.

## Hypercall

```text
RAX = loader key
RCX = command
RDX/R8/R9 = arguments
RAX = return value
```

- Bad key: inject `#UD`;
- Unknown or unsupported command: return `RAX = 3`;
- Command numbers are ABI and may only be appended.

Command families:

- `1..=4`: logging, ping, Stop;
- `5..=7`, `9..=11`, `13..=22`, `0x100`: hooks, EPT native breakpoints, introspection;
- `0x20..=0x26`: `MEM_*`, `SET_PHYS_MEMORY_RANGE`, `SET_SYSTEM_CR3`;
- `0x30..=0x3a`: SVM-only;
- `0x200..=0x206`: reserved (TSC virtualization removed), return `3`.

## Logging

- Only the log ring is active by default; COM1 requires `serial-console`;
- The ring holds 512 entries of 152 bytes; with `log_base == 0` the hypervisor allocates an internal ring from the page pool and the guest drains it with `FLUSH_LOGS`(1);
- Readers use a seqlock; gaps in `id` mean records were overwritten;
- No-serial images are flushed by the loader's periodic thread; crash dumps are searched for `noumen.hv.log`.

## Documentation

- [DOCS.md](DOCS.md): architecture, ABI, implementation details, and pitfalls;
- [loader/windows/kernel/deviceio/README.md](loader/windows/kernel/deviceio/README.md): deviceio IOCTL/command ABI;
- [MCP.md](MCP.md): full MCP/REST hypercall service documentation;
- [README.md](README.md): Chinese overview;

## Known limitations

- Host tests do not cover hardware virtualization paths;
- Raw VMX CPUID passthrough exposes the host VMX/hypervisor bits; noumen still rejects `CR4.VMXE`/`VMXON`;
- NPT has no execute-only support; NPT hooks rely on time-sliced views and are less stealthy than EPT;
- In single-core mode, only the virtualized CPU can execute hypercalls.

## Acknowledgements

- [wbenny/hvpp](https://github.com/wbenny/hvpp)
- [jonomango/hv](https://github.com/jonomango/hv)
- [mikasaw/svmb](https://github.com/mikasaw/svmb)

---

QQ group: 1014181247
