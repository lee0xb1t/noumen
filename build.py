#!/usr/bin/env python3
"""Build noumen and one or both Windows loaders, staging the artifacts.

目录结构（脚本假设的布局）：

    build.py                     本文件
    hypervisor/                  Rust hypervisor crate（产物：noumen ELF）
    loader/windows/deviceio/     设备 IO 版 loader（driver/ + uapp/）
    loader/windows/kdmapper/     手动映射版 loader（driver/ + app/，镜像内嵌）

产物统一落到 `target/<flavor>/`：

    target/deviceio/   noumen, noumen_loader.sys, uapp.exe
    target/kdmapper/   noumen, noumen_kdmapper.sys, uapp.exe

单独构建某一版用 `build_deviceio.py` / `build_kdmapper.py`（见同目录脚本），
它们只是本文件带 `--flavor` 的薄封装。
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent
HYPERVISOR_DIR = REPO_ROOT / "hypervisor"
LOADER_WINDOWS = REPO_ROOT / "loader" / "windows"
PROBE_DIR = REPO_ROOT / "tools" / "rdtsc_probe"

DEFAULT_FEATURES = ["panic-detail", "buffer-log", "vmx-ept", "native-bp", "svm-npt", "mem"]
OPTIONAL_FEATURES = ["serial-console"]
KNOWN_FEATURES = [*OPTIONAL_FEATURES, *DEFAULT_FEATURES]


class BuildError(RuntimeError):
    pass


@dataclass(frozen=True)
class Flavor:
    """一种 Windows loader 的构建/暂存描述。"""

    name: str
    #: 驱动源码目录（含 driver.vcxproj）
    driver_dir: Path
    #: .sys 的期望文件名（vcxproj 的 TargetName / 工程名决定）
    driver_binary: str
    #: 前端 crate 目录（deviceio → um/uapp，kdmapper → um/nsvr）
    app_dir: Path
    #: 暂存时 .sys 使用的名字
    staged_driver: str
    #: 前端可执行文件名（deviceio 是 uapp.exe，kdmapper 是 nsvr.exe）
    app_binary: str = "uapp.exe"
    #: 驱动内嵌 hypervisor 镜像的头文件（相对 driver_dir）；None = 运行时读文件
    embedded_header: str | None = None


FLAVORS: dict[str, Flavor] = {
    "deviceio": Flavor(
        name="deviceio",
        driver_dir=LOADER_WINDOWS / "kernel" / "deviceio",
        driver_binary="noumen_loader.sys",
        app_dir=LOADER_WINDOWS / "um" / "uapp",
        staged_driver="noumen_loader.sys",
    ),
    "kdmapper": Flavor(
        name="kdmapper",
        driver_dir=LOADER_WINDOWS / "kernel" / "kdmapper",
        driver_binary="driver.sys",
        app_dir=LOADER_WINDOWS / "um" / "nsvr",
        staged_driver="noumen_kdmapper.sys",
        app_binary="nsvr.exe",
        embedded_header="noumen_data.h",
    ),
}


def show_step(message: str) -> None:
    print()
    print(f"==> {message}", flush=True)


def normalize_configuration(value: str) -> str:
    normalized = value.strip().lower()
    if normalized == "debug":
        return "Debug"
    if normalized == "release":
        return "Release"
    raise argparse.ArgumentTypeError("--configuration must be Debug or Release")


def normalize_platform(value: str) -> str:
    normalized = value.strip().lower()
    if normalized == "x64":
        return "x64"
    if normalized == "arm64":
        return "ARM64"
    raise argparse.ArgumentTypeError("--platform must be x64 or ARM64")


def profile_of(configuration: str) -> str:
    return "release" if configuration == "Release" else "debug"


def parse_args(argv: list[str]) -> argparse.Namespace:
    feature_lines = "\n".join(f"  {name}" for name in KNOWN_FEATURES)
    default_line = ",".join(DEFAULT_FEATURES)
    parser = argparse.ArgumentParser(
        prog=Path(sys.argv[0]).name,
        description="Build noumen and stage it with a Windows loader.",
        epilog=(
            "Loader flavors:\n"
            "  deviceio  loader/windows/deviceio (IOCTL, image loaded from file at runtime)\n"
            "  kdmapper  loader/windows/kdmapper (image embedded in the driver, manual mapping)\n"
            "  all       both flavors (default)\n\n"
            "Hypervisor features:\n"
            f"{feature_lines}\n\n"
            f"Default: {default_line}\n"
            "Use --features with every feature you need, for example:\n"
            "  python build.py --features "
            f"{default_line}\n"
            "Add serial-console explicitly if COM1 output is required."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        add_help=False,
    )
    parser.add_argument("-h", "--help", action="help", help="show this help and exit")
    parser.add_argument(
        "--release",
        dest="configuration",
        action="store_const",
        const="Release",
        help="build Release (default)",
    )
    parser.add_argument(
        "--debug",
        dest="configuration",
        action="store_const",
        const="Debug",
        help="build Debug",
    )
    parser.add_argument(
        "--configuration",
        type=normalize_configuration,
        help="Debug or Release (default: Release)",
    )
    parser.add_argument(
        "--platform",
        type=normalize_platform,
        default="x64",
        help="driver platform: x64 (default) or ARM64",
    )
    parser.add_argument(
        "--flavor",
        choices=["deviceio", "kdmapper", "all"],
        default="all",
        help="which loader to build (default: all)",
    )
    parser.add_argument(
        "--target-dir",
        help="staging root (default: target); artifacts go to <target-dir>/<flavor>",
    )
    parser.add_argument(
        "--no-serial",
        action="store_true",
        help="build without COM1 serial output",
    )
    parser.add_argument(
        "--features",
        metavar="FEATURES",
        help="explicit hypervisor features, comma separated; disables the default set. "
        "Use --list-features to print every available name",
    )
    parser.add_argument(
        "--list-features",
        action="store_true",
        help="print all available hypervisor features and exit",
    )
    parser.add_argument(
        "--skip-hypervisor",
        action="store_true",
        help="do not rebuild the hypervisor image",
    )
    parser.add_argument(
        "--skip-driver",
        action="store_true",
        help="do not rebuild the loader driver",
    )
    parser.add_argument(
        "--skip-uapp",
        action="store_true",
        help="do not rebuild uapp.exe",
    )
    parser.add_argument(
        "--with-probe",
        action="store_true",
        help="also build rdtsc_probe.exe and copy it next to the loader artifacts",
    )
    parser.add_argument(
        "--hypercall-key",
        default=None,
        metavar="HEX",
        help=(
            "nsvr 的默认 hypercall key（十六进制，可带 0x）；不填用 crate 内置的 0x10F2C。"
            " 只影响 um/nsvr，deviceio 的 uapp 从驱动拿 key"
        ),
    )
    parser.add_argument(
        "--clean",
        action="store_true",
        help="清理所有中间产物后再构建（cargo target、MSBuild build 目录）",
    )
    parser.add_argument(
        "--clean-only",
        action="store_true",
        help="只清理中间产物，不构建；暂存目录 target/<flavor> 保留",
    )
    parser.set_defaults(configuration="Release")

    args = parser.parse_args(argv)

    if args.no_serial and args.features is not None:
        parser.error("--no-serial and --features are mutually exclusive")

    if args.features is not None:
        requested = [item.strip() for item in args.features.split(",") if item.strip()]
        if not requested:
            parser.error("--features requires at least one feature")
        unknown = [item for item in requested if item not in KNOWN_FEATURES]
        if unknown:
            parser.error(
                "unknown feature(s): "
                + ", ".join(unknown)
                + "; available: "
                + ", ".join(KNOWN_FEATURES)
            )
        args.features = ",".join(requested)

    return args


# --------------------------------------------------------------------- 工具发现


def get_cargo_path() -> str:
    cargo = shutil.which("cargo")
    if cargo:
        return cargo
    raise BuildError("cargo was not found in PATH. Install Rust (nightly) and try again.")


def get_msbuild_path() -> str:
    msbuild = shutil.which("msbuild")
    if msbuild:
        return msbuild

    program_files_x86 = os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")
    vswhere = Path(program_files_x86) / "Microsoft Visual Studio" / "Installer" / "vswhere.exe"
    if vswhere.is_file():
        result = subprocess.run(
            [
                str(vswhere),
                "-latest",
                "-products",
                "*",
                "-requires",
                "Microsoft.Component.MSBuild",
                "-find",
                r"MSBuild\**\Bin\MSBuild.exe",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        for line in result.stdout.splitlines():
            candidate = Path(line.strip())
            if candidate.is_file():
                return str(candidate)

    raise BuildError(
        "MSBuild was not found. Install Visual Studio with the WDK "
        "(or run from a Developer PowerShell)."
    )


def run_command(
    command: list[str],
    *,
    cwd: Path,
    env: dict[str, str] | None = None,
    description: str,
) -> None:
    try:
        result = subprocess.run(command, cwd=str(cwd), env=env, check=False)
    except OSError as error:
        raise BuildError(f"{description} failed to start: {error}") from error
    if result.returncode != 0:
        raise BuildError(f"{description} failed with exit code {result.returncode}.")


def find_artifact(candidates: list[Path]) -> Path | None:
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    return None


# ----------------------------------------------------------------------- clean


def directory_size(path: Path) -> int:
    """递归统计大小；权限错误按 0 处理（不因为一个文件读不到就中断清理）。"""
    total = 0

    try:
        for entry in path.rglob("*"):
            try:
                if entry.is_file():
                    total += entry.stat().st_size
            except OSError:
                continue
    except OSError:
        return total

    return total


def remove_tree(path: Path) -> None:
    """删除目录；遇到只读文件或残留句柄时先改权限再重试一次。"""

    def retry(function, failed, _error):
        try:
            os.chmod(failed, 0o700)
            function(failed)
        except OSError:
            pass

    try:
        shutil.rmtree(path)
        return
    except OSError:
        pass

    try:
        shutil.rmtree(path, onerror=retry)
    except TypeError:
        # Python 3.12+ 用 onexc，签名不同。
        shutil.rmtree(path, onexc=lambda function, failed, _error: retry(function, failed, None))


def clean_paths(paths: list[Path]) -> int:
    """删除一批目录/文件，返回释放的字节数。"""
    freed = 0

    for path in paths:
        if not path.exists():
            continue

        freed += directory_size(path) if path.is_dir() else path.stat().st_size

        try:
            remove_tree(path) if path.is_dir() else path.unlink()
        except OSError as error:
            print(f"WARNING: failed to remove {path}: {error}", flush=True)

    return freed


def clean_all() -> None:
    """清理所有中间产物；`target/<flavor>` 暂存目录不动。"""
    show_step("clean")

    targets = [
        HYPERVISOR_DIR / "target",
        REPO_ROOT / "__pycache__",
        # rust-analyzer 的 flycheck 产物（仓库根 target/ 下，只留暂存目录）。
        REPO_ROOT / "target" / "flycheck0",
    ]

    # 每个前端各自就地构建。
    for flavor in FLAVORS.values():
        targets.append(flavor.app_dir / "target")

    # MSBuild 的中间目录（每个 flavor 一份）。
    for flavor in FLAVORS.values():
        targets.append(flavor.driver_dir / "build")
        targets.append(flavor.driver_dir / "x64")
        targets.append(flavor.driver_dir / ".vs")

    freed = clean_paths(targets)

    print(f"cleaned {freed / (1024 * 1024):.1f} MiB", flush=True)


# ------------------------------------------------------------------ hypervisor


def hypervisor_image_path(*, configuration: str, no_serial: bool) -> Path:
    """hypervisor 产物的落点；`--no-serial` 用独立 target 目录，避免互相覆盖。"""
    target = HYPERVISOR_DIR / "target"
    if no_serial:
        target = target / "no-serial"

    return target / "x86_64-unknown-none" / profile_of(configuration) / "noumen"


def build_hypervisor(
    cargo: str,
    *,
    configuration: str,
    features: list[str] | None,
    no_serial: bool,
) -> Path:
    feature_label = f" features: {','.join(features)}" if features else ""
    show_step(f"hypervisor ({configuration}{feature_label})")

    command = [cargo, "build"]
    if configuration == "Release":
        command.append("--release")
    if features:
        command.extend(["--no-default-features", "--features", ",".join(features)])

    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(
        (HYPERVISOR_DIR / "target" / "no-serial") if no_serial else (HYPERVISOR_DIR / "target")
    )
    run_command(
        command,
        cwd=HYPERVISOR_DIR,
        env=env,
        description="cargo build for the hypervisor",
    )

    image = hypervisor_image_path(configuration=configuration, no_serial=no_serial)
    if not image.is_file():
        raise BuildError(f"The hypervisor image was not produced at {image}.")

    return image


def write_embedded_image(image: Path, header: Path) -> bool:
    """把 hypervisor ELF 写成 C 数组头文件；内容没变就不动文件。

    返回是否真的重写（没变时不更新时间戳，避免每次 msbuild 都重编驱动）。
    """
    data = image.read_bytes()
    lines = ["// array size is %d" % len(data), "static const unsigned char noumen_data[] = {"]

    for offset in range(0, len(data), 16):
        chunk = data[offset : offset + 16]
        body = ", ".join(f"0x{byte:02x}" for byte in chunk)
        separator = "," if offset + 16 < len(data) else ""
        lines.append(f"  {body}{separator}")

    lines.append("};")
    text = "\n".join(lines) + "\n"

    if header.is_file() and header.read_text(encoding="utf-8") == text:
        return False

    header.write_text(text, encoding="utf-8", newline="\n")
    return True


# ---------------------------------------------------------------------- driver


def driver_artifact(flavor: Flavor, *, configuration: str, platform: str) -> Path:
    return (
        flavor.driver_dir
        / "build"
        / platform
        / configuration
        / flavor.driver_binary
    )


def build_driver(
    flavor: Flavor,
    *,
    configuration: str,
    platform: str,
    image: Path | None,
) -> Path:
    show_step(f"loader driver [{flavor.name}] ({platform}|{configuration})")

    project = flavor.driver_dir / "driver.vcxproj"
    if not project.is_file():
        raise BuildError(f"missing driver project: {project}")

    if flavor.embedded_header is not None:
        if image is None:
            raise BuildError(
                f"{flavor.name} needs the hypervisor image embedded, but none is available "
                "(do not combine with --skip-hypervisor)"
            )
        header = flavor.driver_dir / flavor.embedded_header
        if write_embedded_image(image, header):
            print(
                f"    embedded image updated: {header.name} <- {image.name} "
                f"({image.stat().st_size:,} B)"
            )

    msbuild = get_msbuild_path()
    command = [
        msbuild,
        str(project),
        f"/p:Configuration={configuration}",
        f"/p:Platform={platform}",
        "/m",
        "/nologo",
        "/v:minimal",
    ]
    result = subprocess.run(command, cwd=str(flavor.driver_dir), check=False)

    driver = driver_artifact(flavor, configuration=configuration, platform=platform)

    if result.returncode != 0:
        if not driver.is_file():
            raise BuildError(f"msbuild failed with exit code {result.returncode}.")
        print(
            f"WARNING: msbuild reported exit {result.returncode} "
            "(inf2cat/InfVerif); using the produced .sys."
        )

    if not driver.is_file():
        raise BuildError(
            f"{flavor.driver_binary} was not produced under "
            f"{flavor.driver_dir / 'build' / platform / configuration}."
        )

    return driver


# ------------------------------------------------------------------------ uapp


def cargo_config_target_dir(crate_dir: Path) -> Path | None:
    """从 crate 的 `.cargo/config.toml` 读 `build.target-dir`（相对 crate 目录）."""
    config = crate_dir / ".cargo" / "config.toml"
    if not config.is_file():
        return None

    for line in config.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if not stripped.startswith("target-dir"):
            continue
        _, _, raw = stripped.partition("=")
        value = raw.strip().strip('"').strip("'")
        if not value:
            return None
        candidate = Path(value)
        return candidate if candidate.is_absolute() else (crate_dir / candidate).resolve()

    return None


def uapp_artifact(flavor: Flavor, *, configuration: str) -> Path:
    """uapp 产物落点。

    两个 app 的 package 都叫 `uapp`，且 `.cargo/config.toml` 都把 `target-dir`
    指到同一个仓库级目录，所以按配置解析；解析不到时退回 crate 自己的 target。
    """
    profile = profile_of(configuration)
    target_dir = cargo_config_target_dir(flavor.app_dir)

    candidates = []
    if target_dir is not None:
        candidates.append(
            target_dir / "x86_64-pc-windows-msvc" / profile / flavor.app_binary
        )
    candidates.append(
        flavor.app_dir / "target" / "x86_64-pc-windows-msvc" / profile / flavor.app_binary
    )
    candidates.append(flavor.app_dir / "target" / profile / flavor.app_binary)

    for candidate in candidates:
        if candidate.is_file():
            return candidate

    # 都没找到时返回首选路径，交给调用方报错。
    return candidates[0]


def build_uapp(
    cargo: str,
    flavor: Flavor,
    *,
    configuration: str,
    hypercall_key: str | None = None,
) -> Path:
    show_step(f"uapp [{flavor.name}] ({configuration})")

    command = [cargo, "build"]
    if configuration == "Release":
        command.append("--release")

    env = os.environ.copy()
    env.pop("CARGO_TARGET_DIR", None)

    # nsvr 的 build.rs 读这个变量当默认 key；uapp 不看它。
    if hypercall_key is not None:
        env["NOUMEN_HYPERCALL_KEY"] = hypercall_key

    run_command(
        command,
        cwd=flavor.app_dir,
        env=env,
        description=f"cargo build for {flavor.name} uapp",
    )

    executable = uapp_artifact(flavor, configuration=configuration)
    if not executable.is_file():
        raise BuildError(f"{flavor.app_binary} was not produced at {executable}.")

    return executable


# --------------------------------------------------------------------- staging


def probe_artifact() -> Path:
    return (
        PROBE_DIR
        / "target"
        / "x86_64-pc-windows-msvc"
        / "release"
        / "rdtsc_probe.exe"
    )


def build_probe(cargo: str) -> Path:
    """构建 RDTSC 时序探针（可选，`--with-probe` 时一起暂存）。"""
    show_step("rdtsc probe (Release)")

    env = os.environ.copy()
    env.pop("CARGO_TARGET_DIR", None)

    run_command(
        [cargo, "build", "--release"],
        cwd=PROBE_DIR,
        env=env,
        description="cargo build for rdtsc_probe",
    )

    executable = probe_artifact()
    if not executable.is_file():
        raise BuildError(f"rdtsc_probe.exe was not produced at {executable}.")

    return executable


def stage(
    flavor: Flavor,
    *,
    root: Path,
    image: Path | None,
    driver: Path | None,
    app: Path | None,
    probe: Path | None = None,
) -> int:
    destination_dir = root / flavor.name
    destination_dir.mkdir(parents=True, exist_ok=True)

    show_step(f"staging [{flavor.name}] -> {destination_dir}")

    staged: list[tuple[str, Path, Path, int]] = []
    missing: list[str] = []
    failed: list[tuple[str, str]] = []

    plan = [
        ("noumen", image),
        (flavor.staged_driver, driver),
        (flavor.app_binary, app),
    ]
    if probe is not None:
        plan.append(("rdtsc_probe.exe", probe))
    for name, source in plan:
        if source is None:
            missing.append(name)
            continue

        destination = destination_dir / name
        try:
            shutil.copy2(source, destination)
            staged.append((name, destination, source, destination.stat().st_size))
        except OSError as error:
            failed.append((name, str(error)))

    for name, destination, source, size in staged:
        print(f"{name:<20} {size:>10,} B  <- {source}")

    for name, error in failed:
        print(f"WARNING: could not overwrite {name}: {error}", file=sys.stderr)

    for name in missing:
        print(f"WARNING: missing artifact: {name}", file=sys.stderr)

    if failed:
        print(
            "WARNING: if noumen is loaded, stop the hypervisor / unload the driver and re-run;"
            "uapp.exe has been updated when possible.",
            file=sys.stderr,
        )

    return 0 if not (missing or failed) else 1


# ------------------------------------------------------------------------ main


def main(argv: list[str]) -> int:
    args = parse_args(argv)

    if args.list_features:
        print("Available hypervisor features:")
        for name in KNOWN_FEATURES:
            print(f"  {name}")
        print()
        print("Default: " + ",".join(DEFAULT_FEATURES))
        return 0

    configuration = args.configuration
    platform = args.platform
    no_serial = args.no_serial

    if args.features is not None:
        features = [item.strip() for item in args.features.split(",") if item.strip()]
    else:
        features = None

    if no_serial:
        features = list(DEFAULT_FEATURES)

    target_root = Path(args.target_dir) if args.target_dir else REPO_ROOT / "target"
    if not target_root.is_absolute():
        target_root = REPO_ROOT / target_root

    selected = list(FLAVORS) if args.flavor == "all" else [args.flavor]

    if args.clean or args.clean_only:
        clean_all()

    if args.clean_only:
        return 0

    # 编译器临时目录放在系统 temp 下，不占用仓库里的任何目录
    # （仓库根 target/ 只放暂存产物）。
    build_temp = Path(tempfile.gettempdir()) / "noumen-build"
    build_temp.mkdir(parents=True, exist_ok=True)
    os.environ["TMP"] = str(build_temp)
    os.environ["TEMP"] = str(build_temp)

    cargo: str | None = None
    if not (args.skip_hypervisor and args.skip_uapp):
        cargo = get_cargo_path()

    # 1) hypervisor：所有 flavor 共用同一份 ELF。
    if args.skip_hypervisor:
        show_step(f"hypervisor ({configuration}, build skipped)")
        image = hypervisor_image_path(configuration=configuration, no_serial=no_serial)
        if not image.is_file():
            print(f"WARNING: build skipped and {image} does not exist")
            image = None
    else:
        assert cargo is not None
        image = build_hypervisor(
            cargo,
            configuration=configuration,
            features=features,
            no_serial=no_serial,
        )

    # 2) 可选：RDTSC 时序探针，所有 flavor 共用一份。
    probe: Path | None = None
    if args.with_probe:
        if cargo is None:
            cargo = get_cargo_path()
        probe = build_probe(cargo)

    # 3) 每个 flavor：驱动 + uapp，然后暂存。
    result = 0
    for flavor_name in selected:
        flavor = FLAVORS[flavor_name]

        if args.skip_driver:
            show_step(f"loader driver [{flavor.name}] ({platform}|{configuration}, build skipped)")
            driver = driver_artifact(flavor, configuration=configuration, platform=platform)
            if not driver.is_file():
                print(f"WARNING: build skipped and {driver} does not exist")
                driver = None
        else:
            driver = build_driver(
                flavor,
                configuration=configuration,
                platform=platform,
                image=image,
            )

        if args.skip_uapp:
            show_step(f"uapp [{flavor.name}] ({configuration}, build skipped)")
            app = uapp_artifact(flavor, configuration=configuration)
            if not app.is_file():
                print(f"WARNING: build skipped and {app} does not exist")
                app = None
        else:
            assert cargo is not None
            app = build_uapp(
                cargo,
                flavor,
                configuration=configuration,
                hypercall_key=args.hypercall_key,
            )

        result |= stage(
            flavor,
            root=target_root,
            image=image,
            driver=driver,
            app=app,
            probe=probe,
        )

    if result == 0:
        print()
        print("Done. Artifacts:")
        for flavor_name in selected:
            print(f"  {target_root / flavor_name}")

    return result


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except BuildError as error:
        print(file=sys.stderr)
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1) from error
