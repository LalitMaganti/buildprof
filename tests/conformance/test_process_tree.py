# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import os
from contextlib import contextmanager
from pathlib import Path

from .model import load_perfetto


@contextmanager
def one_cpu():
    """Run the recorder and the build on one CPU.

    Tracer races that need the build to run between two of the tracer's
    waitpid calls are rare on a machine with many idle CPUs and common on one.
    """
    previous = os.sched_getaffinity(0)
    os.sched_setaffinity(0, {min(previous)})
    try:
        yield
    finally:
        os.sched_setaffinity(0, previous)


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


def test_children_of_new_threads_are_parented_to_their_process(
    run_trace, process_fixture: Path
):
    # Each thread forks as soon as it starts, so the tracer can see a thread's
    # fork before the clone that created the thread. The race needs the
    # threaded program to be a descendant, as in a real build, not the root;
    # the trailing `true` stops sh from exec'ing it in place.
    with one_cpu():
        result, trace = run_trace("sh", "-c", f"{process_fixture} thread-forks 64; true")
    assert result.returncode == 0
    processes = load_perfetto(trace)
    assert_structurally_valid(processes)
    assert len(processes) == 66

    forker = next(
        process
        for process in processes.values()
        if any(
            segment.name == "process-fixture" and "thread-forks" in segment.command
            for segment in process.segments
        )
    )
    children = [p for p in processes.values() if p.parent_pid == forker.pid]
    assert len(children) == 64


def test_threads_that_return_at_once_are_not_recorded_as_processes(
    run_trace, process_fixture: Path
):
    # A thread can exit before the tracer handles the clone that made it.
    # Recording it then as a fork that never exits leaves a phantom process
    # running until the end of the recording.
    with one_cpu():
        result, trace = run_trace("sh", "-c", f"{process_fixture} short-threads 64; true")
    assert result.returncode == 0
    processes = load_perfetto(trace)
    assert_structurally_valid(processes)
    assert len(processes) == 2
