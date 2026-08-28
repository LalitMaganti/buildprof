# Changelog

All notable changes to Buildprof will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-08-22

### Added

- Linux process-tree recording with command, working-directory, and exit-status
  metadata.
- File-open and rename events for reconstructing artifact flow.
- Perfetto trace output with packed process and file tracks.
- Optional Clang and nightly Rust compiler-internal traces.
- Conformance coverage for Make, CMake with Ninja, Meson with Ninja, Cargo, and
  Go builds.
