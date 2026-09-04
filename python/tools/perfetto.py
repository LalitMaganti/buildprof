from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
CONFIG = ROOT / "third_party/perfetto.toml"
CHECKOUT = ROOT / "third_party/src/perfetto"
PATCHES = ROOT / "third_party/patches/perfetto"
OVERLAYS = ROOT / "third_party/overlays/perfetto"


def run(*args: str | Path, cwd: Path | None = None) -> None:
    subprocess.check_call([str(arg) for arg in args], cwd=cwd)


def output(*args: str | Path, cwd: Path | None = None) -> str:
    return subprocess.check_output(
        [str(arg) for arg in args], cwd=cwd, text=True
    ).strip()


def config() -> dict[str, object]:
    with CONFIG.open("rb") as file:
        return tomllib.load(file)["perfetto"]


def patch_files() -> list[Path]:
    return sorted(PATCHES.glob("*.patch"))


def patch_id(contents: bytes) -> str:
    result = subprocess.run(
        ["git", "patch-id", "--stable"],
        input=contents,
        stdout=subprocess.PIPE,
        check=True,
    )
    fields = result.stdout.split()
    return fields[0].decode() if fields else ""


def patches_are_applied() -> bool:
    if not (CHECKOUT / ".git").is_dir():
        return False

    pin = str(config()["pin"])
    try:
        commits = output(
            "git", "rev-list", "--reverse", "--first-parent", f"{pin}..HEAD",
            cwd=CHECKOUT,
        ).splitlines()
        patches = patch_files()
        if len(commits) != len(patches):
            return False

        expected = [patch_id(path.read_bytes()) for path in patches]
        actual = [
            patch_id(subprocess.check_output(
                ["git", "show", "--pretty=format:", "--patch", commit],
                cwd=CHECKOUT,
            ))
            for commit in commits
        ]
        return expected == actual and all(expected)
    except subprocess.CalledProcessError:
        return False


def overlay_files() -> list[tuple[Path, Path]]:
    if not OVERLAYS.exists():
        return []
    return [
        (source, CHECKOUT / source.relative_to(OVERLAYS))
        for source in sorted(OVERLAYS.rglob("*"))
        if source.is_file()
    ]


def ensure_checkout() -> None:
    cfg = config()
    url = str(cfg["url"])
    pin = str(cfg["pin"])
    depth = int(cfg.get("fetch_depth", 300))

    # The directory may already hold a restored toolchain cache (buildtools/),
    # so initialise the repository in place instead of cloning into it.
    CHECKOUT.mkdir(parents=True, exist_ok=True)
    if not (CHECKOUT / ".git").is_dir():
        run("git", "init", "--quiet", CHECKOUT)
        run("git", "remote", "add", "origin", url, cwd=CHECKOUT)
        run("git", "config", "remote.origin.promisor", "true", cwd=CHECKOUT)
        run("git", "config", "remote.origin.partialclonefilter", "blob:none", cwd=CHECKOUT)
    run("git", "remote", "set-url", "origin", url, cwd=CHECKOUT)
    run(
        "git", "fetch", "--depth", str(depth), "--filter=blob:none", "origin", pin,
        cwd=CHECKOUT,
    )
    # Discard local patch commits and edits. Downloaded toolchains and build
    # output are kept: Perfetto's .gitignore does not cover everything CI
    # restores under buildtools/, so exclude both trees explicitly.
    run("git", "checkout", "--detach", "--force", pin, cwd=CHECKOUT)
    run("git", "reset", "--hard", pin, cwd=CHECKOUT)
    run("git", "clean", "-fd", "-e", "buildtools", "-e", "out", cwd=CHECKOUT)


def apply_patches() -> None:
    patches = patch_files()
    if not patches:
        return
    print(f"==> applying {len(patches)} Perfetto patch(es)")
    try:
        run(
            "git",
            "-c", "user.name=buildprof-perfetto",
            "-c", "user.email=buildprof-perfetto@localhost",
            "-c", "commit.gpgsign=false",
            "am", "--keep-cr", "--3way", *patches,
            cwd=CHECKOUT,
        )
    except subprocess.CalledProcessError:
        print(
            "Perfetto patch application stopped with a git-am session open. "
            "Resolve it in third_party/src/perfetto, run `git am --continue`, "
            "then run `tools/perfetto capture`.",
            file=sys.stderr,
        )
        raise


def install_overlays() -> None:
    overlays = overlay_files()
    linked = 0
    for source, destination in overlays:
        target = os.path.relpath(source, destination.parent)
        if destination.is_symlink() and os.readlink(destination) == target:
            continue
        destination.parent.mkdir(parents=True, exist_ok=True)
        if destination.is_dir() and not destination.is_symlink():
            shutil.rmtree(destination)
        elif os.path.lexists(destination):
            destination.unlink()
        destination.symlink_to(target)
        linked += 1
    print(
        f"==> linked {linked} Perfetto overlay file(s)"
        f" ({len(overlays) - linked} already linked)"
    )


def setup() -> None:
    ensure_checkout()
    apply_patches()
    install_overlays()
    print(f"==> Perfetto ready at {CHECKOUT}")


def capture() -> None:
    if not (CHECKOUT / ".git").is_dir():
        sys.exit("Perfetto checkout missing; run `tools/perfetto setup` first")
    pin = str(config()["pin"])
    commits = output(
        "git", "log", "--reverse", "--format=%H %s", f"{pin}..HEAD",
        cwd=CHECKOUT,
    ).splitlines()
    if not commits:
        sys.exit("no commits above the pinned Perfetto revision to capture")

    changed = set(
        output("git", "diff", "--name-only", f"{pin}..HEAD", cwd=CHECKOUT)
        .splitlines()
    )
    overlay_paths = {
        str(source.relative_to(OVERLAYS)) for source, _ in overlay_files()
    }
    overlap = sorted(changed & overlay_paths)
    if overlap:
        sys.exit(
            "permanent overlay paths were committed into the patch stack:\n  "
            + "\n  ".join(overlap)
        )

    dirty = subprocess.check_output(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        cwd=CHECKOUT,
        text=True,
    )
    non_overlay_dirty = []
    for line in dirty.splitlines():
        path = line[3:]
        if path not in overlay_paths:
            non_overlay_dirty.append(line)
    if non_overlay_dirty:
        sys.exit(
            "uncaptured changes remain in the Perfetto checkout:\n  "
            + "\n  ".join(non_overlay_dirty)
        )

    PATCHES.mkdir(parents=True, exist_ok=True)
    for old in PATCHES.glob("*.patch"):
        old.unlink()
    run(
        "git", "format-patch", "-k", "--zero-commit", "--no-signature",
        f"{pin}..HEAD", "-o", PATCHES,
        cwd=CHECKOUT,
    )
    print(f"==> captured {len(commits)} patch commit(s) in {PATCHES}")


def set_pin(pin: str) -> None:
    text = CONFIG.read_text()
    replaced, count = re.subn(
        r'(?m)^pin = "[0-9a-f]{40}"$', f'pin = "{pin}"', text, count=1
    )
    if count != 1:
        sys.exit(f"could not update pin in {CONFIG}")
    CONFIG.write_text(replaced)


def uprev(revision: str) -> None:
    cfg = config()
    url = str(cfg["url"])
    if revision == "latest":
        revision = output("git", "ls-remote", url, "refs/heads/main").split()[0]
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        sys.exit("revision must be a full 40-character SHA or `latest`")
    old = str(cfg["pin"])
    set_pin(revision)
    print(f"==> Perfetto pin {old[:12]} -> {revision[:12]}")
    setup()


UI_OUT = CHECKOUT / "out/buildprof-ui"
UI_DIST = UI_OUT / "ui/dist"


def buildprof_version() -> str:
    """The crate version, which names the UI's deployed `v<version>/` directory."""
    manifest = (ROOT / "Cargo.toml").read_text()
    match = re.search(r'(?m)^version = "([^"]+)"$', manifest)
    if match is None:
        sys.exit("could not read the package version from Cargo.toml")
    return match.group(1)


def build_ui(skip_deps: bool, version_dir: str | None, no_wasm: bool) -> None:
    if patches_are_applied():
        print("==> Perfetto patches already applied; preserving checkout")
        install_overlays()
    else:
        setup()
    if not skip_deps:
        run("python3", "tools/install-build-deps", "--ui", cwd=CHECKOUT)
    version_dir = version_dir or f"v{buildprof_version()}"
    args: list[str | Path] = ["ui/build", "--out", UI_OUT, "--version-dir", version_dir]
    if no_wasm:
        args.append("--no-wasm")
    run(*args, cwd=CHECKOUT)
    print(f"==> self-hostable UI: {UI_DIST} (version directory {version_dir})")


def dev_server(extra: list[str]) -> None:
    """Run Perfetto's watch-and-serve dev server as the Buildprof UI.

    Reuses the wasm modules in the build-ui output directory and applies the
    same version override, so the page reports the Buildprof version rather
    than Perfetto's; `buildprof open --dev-server` points at it.
    """
    if patches_are_applied():
        install_overlays()
    else:
        setup()
    version_dir = f"v{buildprof_version()}"
    os.chdir(CHECKOUT)
    os.execv(
        str(CHECKOUT / "ui/run-dev-server"),
        ["ui/run-dev-server", "--out", str(UI_OUT), "--version-dir", version_dir, *extra],
    )


def package_ui(output_dir: Path | None) -> Path:
    """Bundle the built UI as the release asset the deploy job assembles from.

    The archive holds exactly what one release contributes to the site: the
    root entry page and service worker, plus the versioned asset directory.
    """
    version_dir = f"v{buildprof_version()}"
    if not (UI_DIST / version_dir / "index.html").is_file():
        sys.exit(f"no UI build for {version_dir} in {UI_DIST}; run `tools/perfetto build-ui`")
    output_dir = output_dir or UI_OUT
    output_dir.mkdir(parents=True, exist_ok=True)
    output = output_dir / f"buildprof-ui-{version_dir}.tar.zst"
    members = ["index.html", "service_worker.js", version_dir]
    if (UI_DIST / "service_worker.js.map").is_file():
        members.append("service_worker.js.map")
    with open(output, "wb") as archive:
        tar = subprocess.Popen(
            ["tar", "-C", UI_DIST, "-cf", "-", *members], stdout=subprocess.PIPE
        )
        zstd = subprocess.run(
            ["zstd", "-T0", "-19", "-q"], stdin=tar.stdout, stdout=archive, check=True
        )
        assert tar.stdout is not None
        tar.stdout.close()
        if tar.wait() != 0 or zstd.returncode != 0:
            sys.exit("could not package the UI")
    print(f"==> UI release asset: {output}")
    return output


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Maintain Buildprof's pinned, patched Perfetto UI checkout"
    )
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("setup", help="checkout the pin, apply patches, link overlays")
    sub.add_parser("capture", help="capture pin..HEAD as an ordered patch series")
    uprev_parser = sub.add_parser("uprev", help="change the pin and reapply the stack")
    uprev_parser.add_argument("revision", nargs="?", default="latest")
    build_parser = sub.add_parser("build-ui", help="build the self-hostable UI")
    build_parser.add_argument("--skip-deps", action="store_true")
    build_parser.add_argument(
        "--version-dir", help="override the v<version> directory (default: Cargo.toml)"
    )
    build_parser.add_argument(
        "--no-wasm", action="store_true", help="reuse already-built wasm modules"
    )
    package_parser = sub.add_parser(
        "package-ui", help="archive the built UI as a release asset"
    )
    package_parser.add_argument("--output-dir", type=Path)
    dev_parser = sub.add_parser(
        "dev-server", help="serve the UI with live reload on localhost:10000"
    )
    dev_parser.add_argument("extra", nargs=argparse.REMAINDER,
                            help="arguments passed to ui/run-dev-server")
    args = parser.parse_args()

    if args.command == "setup":
        setup()
    elif args.command == "capture":
        capture()
    elif args.command == "uprev":
        uprev(args.revision)
    elif args.command == "build-ui":
        build_ui(args.skip_deps, args.version_dir, args.no_wasm)
    elif args.command == "package-ui":
        package_ui(args.output_dir)
    elif args.command == "dev-server":
        dev_server(args.extra)
    return 0
