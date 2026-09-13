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
  <a href="docs/investigating-builds.md">Investigation guide</a> ·
  <a href="#install">Install</a>
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
`ninja -C out`. For Bazel, Gradle, Buck2, and other daemon build systems, see
[daemon builds](#bazel-gradle-and-other-daemon-build-systems); for Docker, see
[container builds](#builds-inside-docker-containers).

### 3. Explore the recording

When the build finishes, the recording is saved as `output.buildprof` and
opens in your browser. Allow the one-time prompt to access other apps and
services on this device: that is the page fetching the recording from
localhost. Nothing is ever uploaded.

Start with the longest commands and gaps in parallelism. The
[investigation guide](docs/investigating-builds.md) walks through finding
bottlenecks, following file dependencies, and checking whether a change helped.
For builds over SSH, see [Builds on a remote machine](#builds-on-a-remote-machine).

## Why use Buildprof?

Build tools generally explain only the work they manage themselves. Cargo
timings cannot break down an arbitrary `build.rs` script; Ninja cannot see
inside commands it launches; compiler traces describe one compiler invocation
rather than the build around it.

Buildprof follows the complete process tree, so the same view includes the
build system, compilers, linkers, code generators, and arbitrary tools launched
along the way. You can see how their work fits together and where the build
spends its time.

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

## Usage

### Recording a build

Put `buildprof --` in front of the build command. Choose another output path
or disable automatic opening when needed:

```bash
buildprof -o clean-build.buildprof --no-open -- ninja -C out
```

### Opening recordings

Open an existing recording later with:

```bash
buildprof open clean-build.buildprof
```

buildprof.lalitm.com only delivers the UI itself. Your browser fetches the
recording from localhost and processes it entirely in the page; no trace data
leaves your machine.

Because the page comes from buildprof.lalitm.com and the recording from
localhost, the browser asks once whether the site may access other apps and
services on this device. Allow it. If you block it, the trace never loads. To
recover, allow it again in the site settings next to the address bar, under
"Apps on device" in Chrome or "Access this device" in Firefox, then run
`buildprof open` again.

Recordings contain command lines and filesystem paths. Review them before
sending them to anyone.

### Builds on a remote machine

Over SSH there is no browser to launch, so Buildprof prints the port forward
to run from your own machine instead and waits for the browser to fetch the
trace:

```bash
ssh -L 9001:127.0.0.1:9001 user@buildhost
```

VS Code Remote and JetBrains Gateway forward the port automatically. The wait
gives up after ten minutes; adjust it with `--wait <SECONDS>`, where `0` waits
forever. Alternatively, copy the recording to your own machine and open it
in the [web UI](https://buildprof.lalitm.com). No local installation is needed
for viewing.

### Collection options

Process creation, commands, and timing are always recorded. File opens and
renames are also recorded by default; to reduce overhead on builds with lots
of filesystem activity, disable that layer:

```bash
buildprof --no-file-events -- make -j6
```

The process timeline remains available, but file lists and producer/consumer
links are unavailable. This skips filesystem interception itself, rather than
collecting and discarding events. The UI identifies recordings made this way.

### Compiler details

Process timing is usually the right level for understanding a build. When a
particular compiler or linker invocation needs a closer look, enable compiler
tracing:

```bash
buildprof --compiler-traces -- cargo build
```

Buildprof currently imports Clang `-ftime-trace`, explicitly selected LLD
`--time-trace`, and nightly Rust self-profile data. These events appear as a
summary of active compiler threads with expandable per-thread phase tracks.
Compiler tracing can be combined with process-only recording:

```bash
buildprof --no-file-events --compiler-traces -- ninja -C build
```

A build which invokes Clang through an absolute path currently bypasses
compiler tracing. Buildprof will still record the compiler process, but its
Clang and LLD internal phases will be absent.

Compiler tracing can also change compiler cache keys or turn cache hits into
misses. Existing Rust compiler wrappers remain in the invocation chain, but
cache preservation is not guaranteed in this mode.

## Build systems

Buildprof follows the process tree, so it does not need to understand the
build system. These are exercised by the conformance suite on every change:

- **Make**: compiles, archiving, linking, and renamed outputs.
- **CMake with Ninja**: the configure step and the Ninja build.
- **Meson with Ninja**: the setup step and the Ninja build.
- **Cargo**: `rustc` invocations and linking; nightly self-profile data with
  `--compiler-traces`.
- **Go**: compile, assemble, and link.
- **npm**: offline build scripts, generated JavaScript, and renamed outputs.
- **Bazel**: local actions and generated-file dependencies in batch mode.

Anything else that runs as a child process is recorded the same way: shell
scripts, code generators, wrapper scripts, and tools launched by the build.

## Troubleshooting

Buildprof only records the process tree under the command it runs. Work
outside that tree is not recorded. It usually appears as one long command
with nothing underneath it.

<!-- buildprof.lalitm.com/diagnose/daemons links here; see infra/buildprof.lalitm.com/assemble-site -->
### Bazel, Gradle, and other daemon build systems

Buildprof cannot see work sent to an existing daemon or a remote executor.
An existing daemon leaves only the client in the recording. If the recorded
command starts the daemon, recording continues until the daemon exits.
Disable the daemon or stop it before and after the build:

```sh
bazel shutdown && buildprof -- sh -c 'bazel build //... ; bazel shutdown'
buildprof -- ./gradlew --no-daemon build
buck2 kill && buildprof -- sh -c 'buck2 build //... ; buck2 kill'
sccache --stop-server && buildprof -- sh -c 'cargo build; sccache --stop-server'
```

<!-- buildprof.lalitm.com/diagnose/containers links here; see infra/buildprof.lalitm.com/assemble-site -->
### Builds inside Docker containers

`docker run` hands the container to the Docker daemon. The container's
processes are outside the recorded tree. You can record inside the container
or use Podman.

#### Record inside the container

Download the static musl build on the host and mount it into the container.
It runs in any Linux image without changing the image. Use `aarch64` instead
of `x86_64` for ARM images, including Docker Desktop on Apple Silicon:

```sh
mkdir -p ~/.cache/buildprof-linux
curl -fsSL https://github.com/LalitMaganti/buildprof/releases/latest/download/buildprof-x86_64-unknown-linux-musl.tar.xz \
  | tar -xJ -C ~/.cache/buildprof-linux --strip-components=1
```

Run the build through the mounted binary. Docker needs `--cap-add SYS_PTRACE`
to allow tracing. The recording goes in the mounted project directory:

```sh
docker run --rm --cap-add SYS_PTRACE \
  -v ~/.cache/buildprof-linux:/opt/buildprof:ro \
  -v "$PWD:/src" -w /src my-build-image \
  /opt/buildprof/buildprof -o build.buildprof --no-open -- cargo build
buildprof open build.buildprof
```

Use `--no-open` because the host browser cannot reach the container's
localhost, where the recording would be served. Open the recording with
`buildprof open` on the host. To make the binary a permanent part of the image,
copy it in the Dockerfile.

<!-- buildprof.lalitm.com/diagnose/podman links here; see infra/buildprof.lalitm.com/assemble-site -->
#### Use Podman

Podman has no daemon. Container processes are descendants of `podman run`,
so Buildprof records them without image changes. Podman accepts the same
`run` options as Docker:

```sh
podman info > /dev/null
buildprof -- podman run --rm -v "$PWD:/src" -w /src my-build-image cargo build
```

> [!IMPORTANT]
> Run `podman info` before recording. The first Podman command after a reboot
> or `podman system migrate` sets up the rootless user namespace using setuid
> `newuidmap`. It cannot gain privileges while traced, causing
> `newuidmap: write to uid_map failed: Operation not permitted`.
> Any Podman command run outside the recording sets up the namespace.
> Later commands reuse it.

Commands and file paths from inside the container are recorded as the
container sees them, such as `/src/target/...` rather than the host path.

## How it works

On Linux, Buildprof launches the command under `ptrace` and follows process
creation, execution, and exit through the complete descendant tree. A seccomp
filter lets it stop only for the filesystem operations it records instead of
paying the cost of intercepting every system call.

The recorder writes a Perfetto protobuf trace directly. Perfetto provides the
storage format, query engine, and core timeline interactions; Buildprof adds
the build-specific view on top, including process ancestry, command types,
concurrency, file relationships, and optional compiler timing data.

### Backwards compatibility

Before 1.0, the CLI and the UI move in lockstep at the minor version: a
recording is meant to be viewed in a UI from the same 0.x series, and patch
releases never change the trace format. Every UI version stays deployed under
its own path, the CLI opens the one matching its version, and the UI links to
the matching series when it is handed a recording from a different one, so
nothing stops working, but the trace format may change between minor
releases.

From 1.0 onward, the trace format is stable and compatibility is permanent:
any recording opens in every later UI, and newer UIs simply add features on
top of older recordings.

### Self-hosting the UI

Each release attaches `buildprof-ui-v<version>.tar.zst`, the complete UI as
static files. Serve its contents from any web server and point the CLI at it:

```bash
buildprof open --url https://ui.example.internal/v0.2.0 clean-build.buildprof
```

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for the layout and the UI workflow.
Run the complete conformance suite in the Linux development container:

```bash
just bootstrap
just test
```

For a quick host-side check of formatting, lints, unit tests, and package
contents:

```bash
just release-check
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) and
[AUTHORS](AUTHORS).
