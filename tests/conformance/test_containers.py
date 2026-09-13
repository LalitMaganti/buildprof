# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

"""Record work inside containers started by a daemonless runtime.

`docker run` hands the container to dockerd, outside the recorded process
tree. Podman has no daemon: podman forks conmon, which runs crun, which
starts the container, so its processes should be ordinary descendants.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

from .model import load_file_opens, load_perfetto

IMAGE = "docker.io/library/alpine:3.20"


def _unavailable(message: str) -> None:
    if os.environ.get("BUILDPROF_REQUIRE_TOOLS"):
        pytest.fail(message)
    pytest.skip(message)


@pytest.fixture(scope="module")
def podman() -> str:
    binary = shutil.which("podman")
    if binary is None:
        _unavailable("podman is not installed in this environment")
    # Pull and run once outside the recording. On first use rootless podman
    # maps its user namespace with the setuid newuidmap, which cannot gain
    # privileges under a tracer; later runs join the namespace held by the
    # pause process that first run leaves behind. Under nested cgroups the
    # first container can also fail while podman sets up its parent cgroup.
    warmup = [
        (["pull", "--quiet", IMAGE], 1),
        (["run", "--rm", "--network=none", IMAGE, "true"], 2),
    ]
    for command, attempts in warmup:
        for _ in range(attempts):
            result = subprocess.run(
                [binary, *command], text=True, capture_output=True, timeout=300
            )
            if result.returncode == 0:
                break
        else:
            _unavailable(
                f"podman {command[0]} failed here:\n{result.stdout}{result.stderr}"
            )
    return binary


def _ancestry(processes, pid: int) -> list[str]:
    names = []
    while pid in processes:
        process = processes[pid]
        names.append(process.segments[-1].name)
        pid = process.parent_pid
    return names


def test_podman_container_processes_are_recorded(
    podman: str, buildprof: Path, tmp_path: Path
):
    trace = tmp_path / "podman.pftrace"
    result = subprocess.run(
        [
            str(buildprof),
            "-o",
            str(trace),
            "--no-open",
            "--",
            podman,
            "run",
            "--rm",
            "--network=none",
            "--volume",
            f"{tmp_path}:/out",
            IMAGE,
            "sh",
            "-c",
            "cat /etc/alpine-release > /out/release.txt",
        ],
        cwd=tmp_path,
        text=True,
        capture_output=True,
        timeout=300,
    )
    assert result.returncode == 0, f"podman run failed:\n{result.stdout}{result.stderr}"
    assert (tmp_path / "release.txt").read_text().strip(), "container did not run"

    processes = load_perfetto(trace)
    inside = [
        process
        for process in processes.values()
        if any(s.command == "cat /etc/alpine-release" for s in process.segments)
    ]
    recorded = sorted({s.name for p in processes.values() for s in p.segments})
    assert len(inside) == 1, f"container process missing; recorded: {recorded}"
    cat = inside[0]

    chain = _ancestry(processes, cat.pid)
    assert chain[-1] == "podman", f"cat is not under podman: {' <- '.join(chain)}"

    # The container's own seccomp filter stacks on the recorder's, whose
    # SECCOMP_RET_TRACE outranks its SECCOMP_RET_ALLOW.
    paths = [event.path for event in load_file_opens(trace) if event.pid == cat.pid]
    assert any(path.endswith("/alpine-release") for path in paths), (
        f"no file events from inside the container; cat opened: {paths}"
    )
    assert any(path.endswith("/release.txt") for path in paths), (
        f"no redirect open from inside the container; cat opened: {paths}"
    )
