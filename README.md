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
  <a href="#features">Features</a> ·
  <a href="#install">Install</a> ·
  <a href="#documentation">Documentation</a>
</p>

</div>

## What is Buildprof?

Buildprof traces **every process** a build launches and turns the recording
into an **interactive timeline** you can explore in the browser. Put
`buildprof --` in front of your build command to get started.

- **See the whole build.** Every command the build ran, however it was
  launched, with its command line, working directory, exit status, and place in
  the process tree.
- **Follow the files.** File opens and renames are recorded alongside the
  processes, so the recording links a file's producer to everything that
  consumed it. That link is usually how you find the step waiting on work it
  did not need.
- **Whatever your build system.** Buildprof follows processes rather than build
  systems, so Make, Ninja, CMake, Meson, Cargo, Go, npm, Bazel and Buck2 all
  work, as do the shell scripts, code generators and wrapper scripts they
  launch.

Recording requires Linux or macOS. You can explore recordings on any platform
in the [web UI](https://buildprof.lalitm.com); trace data stays in your
browser.

## Try it in your browser

Explore a [clean ripgrep release build](https://buildprof.lalitm.com/#!/?url=https://buildprof.lalitm.com/examples/ripgrep-release-clean.buildprof)
without installing anything. Click the screenshot to open the recording, or
follow the [guided tour](docs/ripgrep-tutorial.md).

[![A clean ripgrep release build in Buildprof, with the final rustc rg compile selected](docs/assets/ripgrep-release-clean.png)](https://buildprof.lalitm.com/#!/?url=https://buildprof.lalitm.com/examples/ripgrep-release-clean.buildprof)

## Quick start

### 1. Install Buildprof

On the machine that runs your build:

```bash
curl -fsSL https://buildprof.lalitm.com/install.sh | sh
```

Prefer a package manager? See [Install](#install) for Homebrew, mise, Cargo,
and Linux packages.

### 2. Record a build

In your project directory, put `buildprof --` in front of your usual build
command:

```bash
buildprof -- make -j8          # Linux
sudo buildprof -- make -j8     # macOS: kernel tracing needs root
```

Replace `make -j8` with your build command, such as `cargo build` or
`ninja -C out`. On macOS, Buildprof gives up root as soon as tracing is
running, so the build itself runs as you, with your environment, and the
recording belongs to you. Bazel, Gradle and Docker hand work to a daemon,
which needs [a little more care](docs/troubleshooting.md).

### 3. Explore the recording

When the build finishes, the recording is saved as `output.buildprof` and
opens in your browser. Allow the one-time prompt to access other apps and
services on this device: that is the page fetching the recording from
localhost. Nothing is ever uploaded.

Start with the longest commands and gaps in parallelism. The
[investigation guide](docs/investigating-builds.md) walks through finding
bottlenecks, following file dependencies, and checking whether a change helped.

## Features

- **Look inside compilers.** `--compiler-traces` records what a compiler did
  internally, as per-thread phase tracks under the process that produced them,
  so you can go from "this rustc took 40 seconds" to which part of it did.
  ([details](docs/usage.md#compiler-details))
  - Clang `-ftime-trace`: parsing, template instantiation, optimisation.
  - LLD `--time-trace`, when the build selects LLD explicitly.
  - Nightly Rust self-profile data, per compiler query.
- **Record in CI.** The GitHub Action records a build and attaches the
  recording to the job summary, including when the build fails, which is the
  quickest way to find out why CI is slower than your laptop.
  ([details](docs/usage.md#recording-in-github-actions))
- **Record over SSH.** On a build host there is no browser to launch, so
  Buildprof prints the port forward to run from your own machine and waits for
  it. ([details](docs/usage.md#builds-on-a-remote-machine))
- **Turn collection down.** `--no-file-events` keeps the process timeline and
  skips filesystem interception entirely, for builds where that overhead
  matters. ([details](docs/usage.md#collection-options))
- **Keep your build to yourself.** buildprof.lalitm.com delivers the UI and
  nothing else: your browser fetches the recording from localhost and processes
  it in the page. Recordings do contain command lines and paths, so review one
  before sending it to anyone.
- **Check the conformance suite.** Every change records a real build with each
  supported build system and checks the resulting trace.
  ([details](docs/build-systems.md))

## Why use Buildprof?

Build tools generally explain only the work they manage themselves:

- **Cargo timings** cannot break down an arbitrary `build.rs` script.
- **Ninja** cannot see inside the commands it launches.
- **Compiler traces** describe a single compiler invocation rather than the
  build around it.

Buildprof follows the complete process tree instead, so the build system,
compilers, linkers, code generators and whatever else the build launches all
land in one timeline, and you can see how their work fits together.

## Install

Install on your build machine using whichever method you prefer.

<details>
<summary>
Shell installer
</summary>
<p></p>

```bash
curl -fsSL https://buildprof.lalitm.com/install.sh | sh
```
</details>

<details>
<summary>
Homebrew, mise, or Cargo
</summary>
<p></p>

```bash
brew install lalitmaganti/tap/buildprof
mise use -g github:LalitMaganti/buildprof
cargo install --locked buildprof          # builds from source; needs Rust 1.91 or newer
```
</details>

<details>
<summary>
Debian, Ubuntu, Fedora, and other <code>.deb</code> or <code>.rpm</code> distributions
</summary>
<p></p>

Download the package for your architecture from the
[latest release](https://github.com/LalitMaganti/buildprof/releases/latest):

```bash
apt install ./buildprof_*.deb
dnf install ./buildprof-*.rpm
```
</details>

<details>
<summary>
Tarballs
</summary>
<p></p>

The [release page](https://github.com/LalitMaganti/buildprof/releases/latest)
carries prebuilt binaries for x86_64 and aarch64 Linux, both glibc and static
musl. The musl build is the one to mount into a container.
</details>

### Requirements

- **Recording on Linux** needs a kernel and container configuration that
  permits tracing child processes:
  - Docker needs `--cap-add SYS_PTRACE`.
  - `kernel.yama.ptrace_scope` must be below 3.
  - gVisor-style sandboxes cannot trace at all.
- **Recording on macOS** needs macOS 13 or newer, and root to start kernel
  tracing:
  - Run Buildprof with `sudo`; it gives up root once tracing is running.
  - Only one program can use kernel tracing at a time. If Instruments or a
    similar tool holds it, Buildprof says so.
- **Viewing needs nothing.** Recordings open on any platform in the
  [web UI](https://buildprof.lalitm.com), whatever they were recorded on, with
  Buildprof installed or not.
- **Building from source needs Rust 1.91 or newer.**

## Documentation

- Reading a recording:
  - [Investigation guide](docs/investigating-builds.md): finding bottlenecks,
    following dependencies, and checking whether a change helped.
  - [Guided tour](docs/ripgrep-tutorial.md): the example recording, explained.
- Recording one:
  - [Recording and opening builds](docs/usage.md): GitHub Actions, remote
    machines, collection options, compiler details.
  - [Build systems](docs/build-systems.md): what the conformance suite covers.
  - [Troubleshooting](docs/troubleshooting.md): daemon build systems and
    containers.
- Under the hood:
  - [How it works](docs/internals.md): tracing, the trace format, its
    compatibility rules, and self-hosting the UI.
  - [CONTRIBUTING.md](CONTRIBUTING.md): building, testing, and the UI
    workflow.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) and
[AUTHORS](AUTHORS).
