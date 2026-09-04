from __future__ import annotations

import argparse
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
IMAGE = os.environ.get("BUILDPROF_DEV_IMAGE", "buildprof-dev:local")
CONTAINER_NAME = os.environ.get("BUILDPROF_DEV_CONTAINER", "buildprof-dev")
DNS = os.environ.get("BUILDPROF_DEV_DNS", "8.8.8.8")


def container_exists() -> bool:
    result = subprocess.run(
        ["container", "list", "--all", "--quiet"],
        text=True,
        capture_output=True,
        check=True,
    )
    return CONTAINER_NAME in result.stdout.splitlines()


def start_container() -> None:
    if container_exists():
        subprocess.run(
            ["container", "start", CONTAINER_NAME],
            stdout=subprocess.DEVNULL,
            check=True,
        )
    else:
        subprocess.run(
            [
                "container",
                "run",
                "--detach",
                "--name",
                CONTAINER_NAME,
                "--dns",
                DNS,
                "--mount",
                f"type=bind,source={ROOT},target=/work",
                "--workdir",
                "/work",
                IMAGE,
            ],
            stdout=subprocess.DEVNULL,
            check=True,
        )
    print(f"{CONTAINER_NAME} is running")


def container_main() -> int:
    parser = argparse.ArgumentParser(description="Manage the Linux development container")
    parser.add_argument(
        "command", choices=("build", "start", "recreate", "stop", "shell", "status")
    )
    args = parser.parse_args()

    if args.command == "build":
        os.execvp(
            "container",
            [
                "container",
                "build",
                "--dns",
                DNS,
                "--tag",
                IMAGE,
                str(ROOT),
            ],
        )
    if args.command == "start":
        start_container()
        return 0
    if args.command == "recreate":
        subprocess.run(
            ["container", "stop", CONTAINER_NAME],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        subprocess.run(
            ["container", "delete", CONTAINER_NAME],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        start_container()
        return 0
    if args.command == "stop":
        return subprocess.run(["container", "stop", CONTAINER_NAME]).returncode
    if args.command == "shell":
        os.execvp(
            "container",
            ["container", "exec", "--interactive", "--tty", CONTAINER_NAME, "sh"],
        )
    os.execvp("container", ["container", "inspect", CONTAINER_NAME])


def test_main() -> int:
    if not container_exists():
        print(
            "development container is missing; run: "
            "dev/container build && dev/container start",
            file=sys.stderr,
        )
        return 2
    subprocess.run(
        ["container", "start", CONTAINER_NAME],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    os.execvp(
        "container",
        [
            "container",
            "exec",
            CONTAINER_NAME,
            "/work/dev/in-container-test",
            *sys.argv[1:],
        ],
    )


def in_container_test_main() -> int:
    # Runs inside the development container, where ROOT is the /work bind
    # mount, and directly on Linux CI runners, where ROOT is the checkout.
    os.chdir(ROOT)
    environment = os.environ.copy()
    target_dir = Path(environment.get("CARGO_TARGET_DIR", ROOT / "target/linux"))
    environment["CARGO_TARGET_DIR"] = str(target_dir)
    formatting = subprocess.run(
        ["cargo", "fmt", "--all", "--", "--check"], env=environment
    )
    if formatting.returncode != 0:
        return formatting.returncode
    lint = subprocess.run(
        ["cargo", "clippy", "--all-targets", "--locked", "--", "-D", "warnings"],
        env=environment,
    )
    if lint.returncode != 0:
        return lint.returncode
    build = subprocess.run(["cargo", "build", "--locked"], env=environment)
    if build.returncode != 0:
        return build.returncode
    unit_tests = subprocess.run(["cargo", "test", "--locked"], env=environment)
    if unit_tests.returncode != 0:
        return unit_tests.returncode
    environment["BUILDPROF_BIN"] = str(target_dir / "debug/buildprof")
    os.execvpe("pytest", ["pytest", "-q", *sys.argv[1:]], environment)
