# Recording and opening builds

Everything `buildprof` can do beyond `buildprof -- <command>`, which is
covered by the [quick start](../README.md#quick-start).

## Recording a build

Put `buildprof --` in front of the build command. Choose another output path
or disable automatic opening when needed:

```bash
buildprof -o clean-build.buildprof --no-open -- ninja -C out
```

## Recording in GitHub Actions

On a Linux runner, install your build tools, then record the build:

```yaml
steps:
  - uses: actions/checkout@v7
  - uses: LalitMaganti/buildprof@v0.2.7
    with:
      command: |
        cmake -S . -B build
        cmake --build build -j2
```

The job summary links to the recording artifact, including when the build
fails. Download it and open it in [the web UI](https://buildprof.lalitm.com).
See [action inputs](../action.yml) for version and retention settings; use a
unique `artifact-name` for each matrix entry. For upgrades, use release tags
or release SHAs with Dependabot (see [action releases](../RELEASING.md#github-action-releases)).

## Opening recordings

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

## Builds on a remote machine

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

## Collection options

Process creation, commands, and timing are always recorded. File opens and
renames are also recorded by default; to reduce overhead on builds with lots
of filesystem activity, disable that layer:

```bash
buildprof --no-file-events -- make -j6
```

The process timeline remains available, but file lists and producer/consumer
links are unavailable. This skips filesystem interception itself, rather than
collecting and discarding events. The UI identifies recordings made this way.

## Compiler details

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
