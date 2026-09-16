# How Buildprof works

On Linux, Buildprof launches the command under `ptrace` and follows process
creation, execution, and exit through the complete descendant tree. A seccomp
filter lets it stop only for the filesystem operations it records instead of
paying the cost of intercepting every system call.

On macOS, Buildprof reads the kernel's trace events instead. Starting a trace
session needs root. Once the session exists, though, the kernel keeps letting
the process that owns it read from it. Buildprof therefore holds root only
while it starts. It sets up the session and gives up root for good. Only then
does it run the build and write the trace, as the person who started it.

Buildprof gets root by running itself again through `sudo`. It first checks
whether sudo would let it in without a password. If not, it asks for one only
when stdin is a terminal, and otherwise stops with an error. The recording then
runs with `sudo -n`, so it never stops to ask. sudo resets the
environment, so Buildprof first saves its own to a file that only you can
read. The copy that sudo runs reads that file after it has given up root, and
deletes it.

The recorder writes a Perfetto protobuf trace directly. Perfetto provides the
storage format, query engine, and core timeline interactions; Buildprof adds
the build-specific view on top, including process ancestry, command types,
concurrency, file relationships, and optional compiler timing data.

## Backwards compatibility

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

## Self-hosting the UI

Each release attaches `buildprof-ui-v<version>.tar.zst`, the complete UI as
static files. Serve its contents from any web server and point the CLI at it:

```bash
buildprof open --url https://ui.example.internal/v0.2.0 clean-build.buildprof
```
