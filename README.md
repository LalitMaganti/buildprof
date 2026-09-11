# Buildprof

Buildprof shows where the time went in a software build. It traces every
process a Linux build launches and turns the recording into an interactive
timeline you can explore in the browser.

It works below any individual build system, so the same view can include Cargo
crates, Ninja jobs, compiler and linker invocations, shell scripts, code
generators, file access, and arbitrary tools launched along the way.

## Quick start

On the Linux machine that runs the build:

```bash
# Or Homebrew, mise, packages: see Install below.
curl -fsSL https://buildprof.lalitm.com/install.sh | sh

# Your build command after --.
buildprof -- make -j8
```

When the build finishes, the recording is saved as `output.buildprof` and
opens in your browser at [buildprof.lalitm.com](https://buildprof.lalitm.com).
**Nothing is ever uploaded**: the page fetches the recording from localhost,
which is why the browser asks once for permission to access other apps and
services on this device. Allow it. See
[Opening recordings](#opening-recordings) for details.

Any build system works (\*). See [Build systems](#build-systems) for what is
recorded.

To see the result without installing anything, open the pre-recorded
[ripgrep release build](https://buildprof.lalitm.com/#!/?url=https://buildprof.lalitm.com/examples/ripgrep-release-clean.buildprof)
in the browser:

![A clean ripgrep release build opened in Buildprof](docs/assets/ripgrep-release-clean.png)

(\*) Build systems that use a daemon, such as Bazel, Gradle, and Buck2, need to
be run a little differently; see [Daemon build systems](#daemon-build-systems).

## Why use Buildprof?

Build tools generally explain only the work they manage themselves. Cargo
timings cannot break down an arbitrary `build.rs` script; Ninja cannot see
inside commands it launches; compiler traces describe one compiler invocation
rather than the build around it.

Buildprof follows the complete process tree instead. It lets you:

- see where wall-clock time went across the whole build;
- spot work which ran serially, overlapped, or started unexpectedly late;
- inspect full commands, working directories, lifetimes, and exit statuses;
- follow files from the process which produced them to processes which read
  them;
- use the same profiler with Make, Ninja, CMake, Meson, Cargo, Go, or wrapper
  scripts; and
- optionally add compiler-internal phases from Clang, LLD, and nightly Rust.

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

### Investigating a slow build

See the [investigation guide](docs/investigating-builds.md) to find expensive
commands, follow their inputs, and inspect compiler phases.

## Build systems

Buildprof follows the process tree, so it does not need to understand the
build system. These are exercised by the conformance suite on every change:

- **Make**: compiles, archiving, linking, and renamed outputs.
- **CMake with Ninja**: the configure step and the Ninja build.
- **Meson with Ninja**: the setup step and the Ninja build.
- **Cargo**: `rustc` invocations and linking; nightly self-profile data with
  `--compiler-traces`.
- **Go**: compile, assemble, and link.

Anything else that runs as a child process is recorded the same way: shell
scripts, code generators, wrapper scripts, and tools launched by the build.

### Daemon build systems

Work handed to a daemon or a remote executor happens outside the process tree
and is not visible. If the daemon is already running, the recording shows only
the client; if the recorded command starts it, the recording continues until
the daemon exits. Run without the daemon, or stop it before and after the
build:

```sh
bazel shutdown && buildprof -- sh -c 'bazel build //... ; bazel shutdown'
buildprof -- ./gradlew --no-daemon build
buck2 kill && buildprof -- sh -c 'buck2 build //... ; buck2 kill'
sccache --stop-server && buildprof -- sh -c 'cargo build; sccache --stop-server'
```

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
