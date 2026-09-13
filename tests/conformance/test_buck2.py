# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

"""Record local Buck2 actions and their daemon-mediated artifact flow."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path
from textwrap import dedent

import pytest

from .model import dependency_edges, load_perfetto


def test_buck2_build(buildprof: Path, tmp_path: Path):
    for tool in ("buck2", "cp"):
        if shutil.which(tool) is None:
            message = f"{tool} is not installed in this environment"
            if os.environ.get("BUILDPROF_REQUIRE_TOOLS"):
                pytest.fail(message)
            pytest.skip(message)

    project = tmp_path / "project"
    project.mkdir()
    (project / "input.js").write_text("module.exports = 3;\n")
    (project / ".buckconfig").write_text(dedent("""\
        [cells]
          root = .
        [cell_aliases]
          config = root
        [buildfile]
          name = BUCK
        """))
    # Custom rules need neither a downloaded prelude nor language toolchains.
    # Local execution keeps both actions in the recorded process tree.
    copy_tool = json.dumps(shutil.which("cp"))
    (project / "copy.bzl").write_text(dedent(f"""\
        def _copy_impl(ctx):
            out = ctx.actions.declare_output(ctx.attrs.out)
            ctx.actions.run(
                [{copy_tool}, ctx.attrs.src, out.as_output()],
                category = "copy",
                local_only = True,
            )
            return [DefaultInfo(default_output = out)]

        copy = rule(
            impl = _copy_impl,
            attrs = {{
                "src": attrs.source(),
                "out": attrs.string(),
            }},
        )
        """))
    (project / "BUCK").write_text(dedent("""\
        load(":copy.bzl", "copy")

        copy(name = "generate", src = "input.js", out = "generated.js")
        copy(name = "bundle", src = ":generate", out = "output.js")
        """))

    # A fresh project forces both actions to run. Start and stop its isolated
    # daemon inside the recording, preserving the build's status on failure.
    command = dedent("""\
        buck2 --isolation-dir buildprof-conformance build //:bundle --out output.js
        build_status=$?
        buck2 --isolation-dir buildprof-conformance kill || exit "$?"
        exit "$build_status"
        """)
    trace = tmp_path / "buck2.pftrace"
    result = subprocess.run(
        [
            str(buildprof), "--no-open", "-o", str(trace), "--",
            "sh", "-c", command,
        ],
        cwd=project,
        env=dict(os.environ, LC_ALL="C"),
        text=True,
        capture_output=True,
        timeout=180,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert (project / "output.js").read_text() == "module.exports = 3;\n"
    processes = load_perfetto(trace)

    def action_pid(output: str) -> int:
        matches = [
            process.pid
            for process in processes.values()
            if any(
                segment.name == "cp" and segment.command.endswith(f"/{output}")
                for segment in process.segments
            )
        ]
        assert len(matches) == 1, f"expected one copy action for {output}: {matches}"
        return matches[0]

    producer = action_pid("generated.js")
    consumer = action_pid("output.js")
    assert producer != consumer
    edges = [
        edge for edge in dependency_edges(trace)
        if edge.path.endswith("/generated.js")
    ]
    # Buck2 may copy the producer's output to a content-addressed path in its
    # daemon. Accept either the direct edge or both edges through that copy.
    readers = {edge.consumer_pid for edge in edges if edge.producer_pid == producer}
    writers = {edge.producer_pid for edge in edges if edge.consumer_pid == consumer}
    assert consumer in readers or readers & writers, (
        f"generated-file flow missing: {edges}"
    )
