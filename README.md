<div align="center">

<h1><a href="https://buildprof.lalitm.com">Buildprof</a></h1>

<p><strong>See where the time went in your build.</strong></p>

<p>
  <a href="https://crates.io/crates/buildprof"><img alt="Crates.io version" src="https://img.shields.io/crates/v/buildprof?style=for-the-badge"></a>
  <a href="LICENSE"><img alt="License: Apache-2.0" src="https://img.shields.io/badge/license-Apache--2.0-blue?style=for-the-badge"></a>
  <a href="https://github.com/LalitMaganti/buildprof/actions/workflows/ci.yml"><img alt="CI status" src="https://img.shields.io/github/actions/workflow/status/LalitMaganti/buildprof/ci.yml?branch=main&style=for-the-badge"></a>
</p>

<p>
  <a href="#quick-start">Quick start</a> ·
  <a href="https://buildprof.lalitm.com/#!/?url=https://buildprof.lalitm.com/examples/ripgrep-release-clean.buildprof">Try the demo</a> ·
  <a href="#install">Install</a> ·
  <a href="#documentation">Documentation</a>
</p>

</div>

## What is Buildprof?

Buildprof traces every process a Linux build launches and turns the recording
into an interactive timeline you can explore in the browser. Put `buildprof --`
in front of your build command to get started.

- **See the whole build.** Find expensive commands, gaps in parallelism, and
  work that starts unexpectedly late across Make, Ninja, CMake, Meson, Cargo,
  Go, and shell scripts.
- **Follow the files.** Inspect commands, working directories, and exit statuses,
  then follow inputs back to the processes that produced them.
- **Look inside compilers.** Optionally add internal timing data from Clang,
  LLD, and nightly Rust alongside the process timeline.

Build tools generally explain only the work they manage themselves. Cargo
timings cannot break down an arbitrary `build.rs` script; Ninja cannot see
inside commands it launches; compiler traces describe one compiler invocation
rather than the build around it. Buildprof follows the complete process tree,
so the same view covers the build system, compilers, linkers, code generators,
and anything else the build launches.

Recording requires Linux. You can explore recordings on any platform in the
[web UI](https://buildprof.lalitm.com); trace data stays in your browser.

## Try it in your browser

Explore a [clean ripgrep release build](https://buildprof.lalitm.com/#!/?url=https://buildprof.lalitm.com/examples/ripgrep-release-clean.buildprof)
without installing anything. Click the screenshot to open the recording, or
follow the [guided tour](docs/ripgrep-tutorial.md).

[![A clean ripgrep release build in Buildprof, with the final rustc rg compile selected](docs/assets/ripgrep-release-clean.png)](https://buildprof.lalitm.com/#!/?url=https://buildprof.lalitm.com/examples/ripgrep-release-clean.buildprof)

## Quick start

### 1. Install Buildprof

On the Linux machine that runs your build:

```bash
curl -fsSL https://buildprof.lalitm.com/install.sh | sh
```

Prefer a package manager? See [Install](#install) for Homebrew, mise, Cargo,
and Linux packages.

### 2. Record a build

In your project directory, put `buildprof --` in front of your usual build
command:

```bash
buildprof -- make -j8
```

Replace `make -j8` with your build command, such as `cargo build` or
`ninja -C out`. Bazel, Gradle and Docker hand work to a daemon, which needs
[a little more care](docs/troubleshooting.md).

### 3. Explore the recording

When the build finishes, the recording is saved as `output.buildprof` and
opens in your browser. Allow the one-time prompt to access other apps and
services on this device: that is the page fetching the recording from
localhost. Nothing is ever uploaded.

Start with the longest commands and gaps in parallelism. The
[investigation guide](docs/investigating-builds.md) walks through finding
bottlenecks, following file dependencies, and checking whether a change helped.

## Install

Install on your Linux build machine using whichever method you prefer.

**Shell installer**:

```bash
curl -fsSL https://buildprof.lalitm.com/install.sh | sh
```

**Homebrew**:

```bash
brew install lalitmaganti/tap/buildprof
```

**mise**:

```bash
mise use -g github:LalitMaganti/buildprof
```

**Cargo** (builds from source; needs Rust 1.91 or newer):

```bash
cargo install --locked buildprof
```

**Debian, Ubuntu, Fedora, and other `.deb` or `.rpm` distributions**: download
the package for your architecture from the
[latest release](https://github.com/LalitMaganti/buildprof/releases/latest)
and install it with `apt install ./buildprof_*.deb` or
`dnf install ./buildprof-*.rpm`.

**Tarballs**: the same release page carries prebuilt binaries for x86_64 and
aarch64 Linux, both glibc and static musl.

### Requirements

**Recording builds is currently supported on Linux only.** The kernel or
container configuration must permit tracing child processes: Docker needs
`--cap-add SYS_PTRACE`, `kernel.yama.ptrace_scope` must be below 3, and
gVisor-style sandboxes cannot trace at all. Installing from source needs Rust
1.91 or newer.

Existing recordings can be viewed on any platform in the
[web UI](https://buildprof.lalitm.com), without installing Buildprof. Viewing a
recording does not require the same operating system it was recorded on.

## Documentation

- [Investigation guide](docs/investigating-builds.md) — finding bottlenecks in
  a recording.
- [Guided tour](docs/ripgrep-tutorial.md) — the example recording, explained.
- [Recording and opening builds](docs/usage.md) — GitHub Actions, remote
  machines, collection options, compiler details.
- [Build systems](docs/build-systems.md) — what the conformance suite covers.
- [Troubleshooting](docs/troubleshooting.md) — daemon build systems and
  containers.
- [How it works](docs/internals.md) — tracing, the trace format, compatibility,
  and self-hosting the UI.
- [CONTRIBUTING.md](CONTRIBUTING.md) — building, testing, and the UI workflow.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) and
[AUTHORS](AUTHORS).
