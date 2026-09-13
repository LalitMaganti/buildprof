# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

"""Record a local Bazel action graph without fetching external build rules."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess

import pytest

from .model import dependency_edges, load_perfetto


def test_bazel_build(buildprof: Path, tmp_path: Path):
    for tool in ("bazel", "cp"):
        if shutil.which(tool) is None:
            message = f"{tool} is not installed in this environment"
            if os.environ.get("BUILDPROF_REQUIRE_TOOLS"):
                pytest.fail(message)
            pytest.skip(message)

    project = tmp_path / "project"
    project.mkdir()
    (project / "input.js").write_text("module.exports = 3;\n")
    (project / "MODULE.bazel").write_text("")
    # Custom rules exercise real actions and artifact flow without downloading
    # language rules or toolchains. Use local execution so actions remain in
    # the recorded process tree and need no sandbox/user-namespace privileges.
    copy_tool = json.dumps(shutil.which("cp"))
    (project / "copy.bzl").write_text(
        "def _copy_impl(ctx):\n"
        "    out = ctx.actions.declare_file(ctx.attr.out)\n"
        f"    ctx.actions.run(executable = {copy_tool},\n"
        "                    arguments = [ctx.file.src.path, out.path],\n"
        "                    inputs = [ctx.file.src], outputs = [out],\n"
        "                    mnemonic = \"Copy\")\n"
        "    return [DefaultInfo(files = depset([out]))]\n"
        "copy = rule(implementation = _copy_impl, attrs = {\n"
        "    \"src\": attr.label(allow_single_file = True, mandatory = True),\n"
        "    \"out\": attr.string(mandatory = True),\n"
        "})\n"
    )
    # Avoid the default host platform, which fetches @platforms even for
    # language-independent rules. These local copy actions need no constraints.
    (project / "BUILD.bazel").write_text(
        'load(":copy.bzl", "copy")\n'
        'platform(name = "local")\n'
        'copy(name = "generate", src = "input.js", out = "generated.js")\n'
        'copy(name = "bundle", src = ":generate", out = "output.js")\n'
    )
    trace = tmp_path / "bazel.pftrace"
    result = subprocess.run(
        [str(buildprof), "--no-open", "-o", str(trace), "--",
         "bazel", "--batch", f"--output_user_root={tmp_path / 'cache'}",
         "--ignore_all_rc_files", "build", "--spawn_strategy=local",
         "--host_platform=//:local", "--platforms=//:local",
         "--repository_disable_download",
         f"--repository_cache={tmp_path / 'repositories'}", "//:bundle"],
        cwd=project,
        env=dict(os.environ, LC_ALL="C"),
        text=True, capture_output=True, timeout=180,
    )
    # A fresh output root forces both actions to run; batch mode starts no
    # persistent daemon and leaves the user's existing Bazel server alone.
    assert result.returncode == 0, result.stdout + result.stderr
    assert (project / "bazel-bin/output.js").read_text() == "module.exports = 3;\n"
    processes = load_perfetto(trace)

    def action_pid(output: str) -> int:
        matches = [
            process.pid
            for process in processes.values()
            if any(segment.name == "cp" and segment.command.endswith(f"/{output}")
                   for segment in process.segments)
        ]
        assert len(matches) == 1, f"expected one copy action for {output}: {matches}"
        return matches[0]

    producer = action_pid("generated.js")
    consumer = action_pid("output.js")
    assert producer != consumer
    assert any(
        edge.producer_pid == producer and edge.consumer_pid == consumer
        and edge.path.endswith("/generated.js")
        for edge in dependency_edges(trace)
    )
