# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

from pathlib import Path
import subprocess

from .model import load_perfetto


def root_of(processes):
    roots = [process for process in processes.values() if process.parent_pid == 0]
    assert len(roots) == 1
    return roots[0]


def assert_structurally_valid(processes):
    assert processes
    roots = 0
    for process in processes.values():
        assert process.segments
        if process.parent_pid == 0:
            roots += 1
        else:
            assert process.parent_pid in processes
        for segment in process.segments:
            assert segment.duration_ns > 0
            assert segment.name
    assert roots == 1


def test_single_process_has_required_metadata(run_trace, process_fixture: Path, tmp_path: Path):
    result, trace = run_trace(process_fixture, "single")
    assert result.returncode == 0
    processes = load_perfetto(trace)
    assert_structurally_valid(processes)
    assert len(processes) == 1

    process = root_of(processes)
    assert len(process.segments) == 1
    segment = process.segments[0]
    assert "single" in segment.command
    assert segment.cwd == str(tmp_path)
    assert segment.exit_code == 0


def test_fork_and_exec_child_is_nested_under_root(run_trace, process_fixture: Path):
    result, trace = run_trace(process_fixture, "fork-exec")
    assert result.returncode == 0
    processes = load_perfetto(trace)
    assert_structurally_valid(processes)
    assert len(processes) == 2

    root = root_of(processes)
    child = next(process for process in processes.values() if process.pid != root.pid)
    assert child.parent_pid == root.pid
    assert any("leaf" in segment.command for segment in child.segments)


def test_child_that_never_execs_is_still_recorded(run_trace, process_fixture: Path):
    result, trace = run_trace(process_fixture, "fork-no-exec")
    assert result.returncode == 0
    processes = load_perfetto(trace)
    assert_structurally_valid(processes)
    assert len(processes) == 2

    root = root_of(processes)
    child = next(process for process in processes.values() if process.pid != root.pid)
    assert child.parent_pid == root.pid


def test_exec_chain_is_consecutive_segments_on_one_track(run_trace, process_fixture: Path):
    result, trace = run_trace(process_fixture, "exec-chain")
    assert result.returncode == 0
    processes = load_perfetto(trace)
    assert_structurally_valid(processes)
    assert len(processes) == 1

    segments = root_of(processes).segments
    assert len(segments) == 2
    assert "exec-chain" in segments[0].command
    assert "exec-leaf" in segments[1].command
    assert segments[0].end_ns <= segments[1].start_ns


def test_parallel_children_overlap(run_trace, process_fixture: Path):
    result, trace = run_trace(process_fixture, "parallel", "4")
    assert result.returncode == 0
    processes = load_perfetto(trace)
    assert_structurally_valid(processes)
    assert len(processes) == 5

    root = root_of(processes)
    children = [process for process in processes.values() if process.pid != root.pid]
    assert all(child.parent_pid == root.pid for child in children)
    assert max(child.start_ns for child in children) < min(child.end_ns for child in children)


def test_threads_dying_mid_file_open_do_not_end_the_recording(buildprof: Path, tmp_path: Path):
    # Threads opening files when their process exits vanish in a ptrace stop;
    # test suites do this constantly.
    race = tmp_path / "race.py"
    race.write_text(
        "import os, threading, time\n"
        "def spin():\n"
        "    while True:\n"
        "        try: open('/etc/hostname').close()\n"
        "        except OSError: pass\n"
        "for _ in range(16): threading.Thread(target=spin, daemon=True).start()\n"
        "time.sleep(0.05); os._exit(0)\n"
    )
    script = f"for i in 1 2 3 4 5 6 7 8; do python3 {race} & done; wait"
    for attempt in range(10):
        result = subprocess.run(
            [str(buildprof), "--no-open", "-o", str(tmp_path / "race.buildprof"), "--", "sh", "-c", script],
            cwd=tmp_path, text=True, capture_output=True, timeout=60,
        )
        assert result.returncode == 0, f"attempt {attempt}: {result.stderr}"
