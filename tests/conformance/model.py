from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from perfetto.trace_processor import TraceProcessor


@dataclass(frozen=True)
class Segment:
    name: str
    command: str
    cwd: str
    start_ns: int
    duration_ns: int
    pid: int
    parent_pid: int
    exit_code: int | None
    execed: bool

    @property
    def end_ns(self) -> int:
        return self.start_ns + self.duration_ns


@dataclass(frozen=True)
class FileOpen:
    path: str
    timestamp_ns: int
    pid: int
    flags: int
    fd: int


@dataclass(frozen=True)
class Rename:
    from_path: str
    to_path: str
    timestamp_ns: int
    pid: int


@dataclass
class Process:
    pid: int
    parent_pid: int
    segments: list[Segment]

    @property
    def start_ns(self) -> int:
        return min(segment.start_ns for segment in self.segments)

    @property
    def end_ns(self) -> int:
        return max(segment.end_ns for segment in self.segments)


def load_perfetto(path: Path) -> dict[int, Process]:
    processor = TraceProcessor(trace=str(path))
    try:
        importer_errors = list(
            processor.query(
                "select name, value from stats "
                "where value > 0 and name like '%error%'"
            )
        )
        assert not importer_errors, f"Perfetto importer errors: {importer_errors}"

        rows = list(
            processor.query(
                """
                select
                  s.name,
                  s.ts,
                  s.dur,
                  extract_arg(s.arg_set_id, 'debug.cmd') as command,
                  extract_arg(s.arg_set_id, 'debug.cwd') as cwd,
                  extract_arg(s.arg_set_id, 'debug.pid') as pid,
                  extract_arg(s.arg_set_id, 'debug.ppid') as parent_pid,
                  extract_arg(s.arg_set_id, 'debug.exit_code') as exit_code,
                  extract_arg(s.arg_set_id, 'debug.execed') as execed
                from slice s
                where extract_arg(s.arg_set_id, 'debug.pid') is not null
                order by s.ts
                """
            )
        )
    finally:
        processor.close()

    grouped: dict[int, list[Segment]] = {}
    for row in rows:
        segment = Segment(
            name=str(row.name),
            command=str(row.command or ""),
            cwd=str(row.cwd or ""),
            start_ns=int(row.ts),
            duration_ns=int(row.dur),
            pid=int(row.pid),
            parent_pid=int(row.parent_pid),
            exit_code=(int(row.exit_code) if row.exit_code is not None else None),
            # Absent in traces recorded before fork-only processes were kept;
            # everything in those did exec.
            execed=bool(row.execed) if row.execed is not None else True,
        )
        grouped.setdefault(segment.pid, []).append(segment)

    processes: dict[int, Process] = {}
    for pid, segments in grouped.items():
        parent_ids = {segment.parent_pid for segment in segments}
        assert len(parent_ids) == 1, f"process {pid} changes parent: {parent_ids}"
        processes[pid] = Process(pid, parent_ids.pop(), segments)
    return processes


def load_file_opens(path: Path) -> list[FileOpen]:
    processor = TraceProcessor(trace=str(path))
    try:
        rows = list(
            processor.query(
                """
                select
                  s.ts,
                  extract_arg(s.arg_set_id, 'debug.path') as path,
                  extract_arg(s.arg_set_id, 'debug.owner_pid') as pid,
                  extract_arg(s.arg_set_id, 'debug.flags') as flags,
                  extract_arg(s.arg_set_id, 'debug.fd') as fd
                from slice s
                where s.category = 'buildprof.file' and s.name = 'open'
                order by s.ts
                """
            )
        )
    finally:
        processor.close()

    return [
        FileOpen(
            path=str(row.path),
            timestamp_ns=int(row.ts),
            pid=int(row.pid),
            flags=int(row.flags),
            fd=int(row.fd),
        )
        for row in rows
    ]


def load_renames(path: Path) -> list[Rename]:
    processor = TraceProcessor(trace=str(path))
    try:
        rows = list(
            processor.query(
                """
                select
                  s.ts,
                  extract_arg(s.arg_set_id, 'debug.from') as from_path,
                  extract_arg(s.arg_set_id, 'debug.to') as to_path,
                  extract_arg(s.arg_set_id, 'debug.owner_pid') as pid
                from slice s
                where s.category = 'buildprof.rename'
                order by s.ts
                """
            )
        )
    finally:
        processor.close()
    return [
        Rename(
            from_path=str(row.from_path),
            to_path=str(row.to_path),
            timestamp_ns=int(row.ts),
            pid=int(row.pid),
        )
        for row in rows
    ]


# The artifact graph, defined once here and pinned by the build-system diff
# tests. The UI computes the same thing in SQL; this is the specification.

ARTIFACT_EXTENSIONS = frozenset(
    """
    o obj a lib so dylib dll rlib rmeta bc ll s asm pch gch
    h hh hpp hxx inc c cc cpp cxx rs go zig ts js proto
    """.split()
)


def _extension(path: str) -> str:
    base = path.rsplit("/", 1)[-1]
    if "." not in base:
        return ""
    return base.rsplit(".", 1)[-1].lower()


def _is_write(flags: int) -> bool:
    # O_WRONLY/O_RDWR live in the low two bits; O_CREAT is 0o100.
    return bool(flags & 0o3) or bool(flags & 0o100)


@dataclass(frozen=True)
class Edge:
    """One action produced a file another action consumed."""

    producer_pid: int
    consumer_pid: int
    path: str


def dependency_edges(path: Path) -> list[Edge]:
    """Resolve producer/consumer edges through renamed outputs."""
    opens = load_file_opens(path)
    renames = load_renames(path)
    moved = {rename.from_path: rename.to_path for rename in renames}

    writers: dict[str, set[int]] = {}
    readers: dict[str, set[int]] = {}
    for entry in opens:
        target = moved.get(entry.path, entry.path)
        if _extension(target) not in ARTIFACT_EXTENSIONS:
            continue
        side = writers if _is_write(entry.flags) else readers
        side.setdefault(target, set()).add(entry.pid)

    edges = []
    for artifact, producing in writers.items():
        for producer in producing:
            for consumer in readers.get(artifact, ()):
                if producer != consumer:
                    edges.append(Edge(producer, consumer, artifact))
    return sorted(edges, key=lambda e: (e.path, e.producer_pid, e.consumer_pid))
