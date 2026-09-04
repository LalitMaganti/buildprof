# Changelog

All notable changes to Buildprof will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-09-04

### Added

- `buildprof.version` and `buildprof.trace_format` trace attributes, visible
  in Trace Processor's `metadata` table, so the UI can tell which recorder
  wrote a trace.
- SSH sessions print the `ssh -L` port forward to run from your own machine
  instead of trying to launch a browser on the build host.
- `--wait <SECONDS>` bounds how long Buildprof waits for a browser to fetch a
  trace (default 600; 0 waits forever).
- `buildprof examples` and `buildprof open --example <NAME>` open recordings
  hosted next to the UI.
- Clear messages when ptrace or seccomp is unavailable (Docker without
  `SYS_PTRACE`, Yama `ptrace_scope` 3, gVisor), when the command cannot be
  executed, and when port 9001 is already taken.
- Prebuilt binaries on GitHub Releases for x86_64 and aarch64 Linux (glibc
  2.28 or newer, plus static musl builds) and for macOS, with a shell
  installer, a Homebrew tap, `.deb` and `.rpm` packages, and mise support.
- Every released UI is deployed permanently under
  `https://buildprof.lalitm.com/v<version>/`; the CLI opens the UI matching
  its own version and the UI links to the matching version when a trace was
  recorded by a different release.

### Changed

- Traces are now written as a single zstd stream, typically 9 to 20 times
  smaller than before. The UI detects the compression from the file's magic
  bytes and the `.buildprof` extension is unchanged. Querying a trace directly
  needs a Trace Processor build from July 2026 or later; the `perfetto` Python
  package's bundled build is older, so pass
  `TraceProcessorConfig(fetch_latest_trace_processor=True)` for now.
- Building from source now requires Rust 1.91.

## [0.1.0] - 2026-08-22

### Added

- Linux process-tree recording with command, working-directory, and exit-status
  metadata.
- File-open and rename events for reconstructing artifact flow.
- Perfetto trace output with packed process and file tracks.
- Optional Clang and nightly Rust compiler-internal traces.
- Conformance coverage for Make, CMake with Ninja, Meson with Ninja, Cargo, and
  Go builds.
