# Buildprof

Buildprof records a Linux build and its descendant processes as an interactive
trace. It captures process lifetimes, command lines, working directories, exit
statuses, file opens, and renamed outputs so you can see where a build spent
its time and how artifacts moved between tools.

Buildprof is build-system agnostic: if the work runs in a descendant process,
it can be recorded. GNU Make, CMake with Ninja, Meson with Ninja, Cargo, and Go
are covered by the conformance suite.

## Requirements

- Linux (the recorder uses `ptrace` and seccomp)
- Rust 1.85 or newer when installing from source
- A kernel or container configuration that permits tracing child processes

Builds delegated to an existing daemon or another machine are outside the
recorded process tree. Use a build system's no-daemon or local-execution mode
when one is available.

## Install

From crates.io:

```console
cargo install buildprof --locked
```

From a source checkout:

```console
cargo install --path . --locked
```

## Use

Put the command after `--` so its options pass through unchanged:

```console
buildprof -- make -j8
```

The default output is `output.buildprof`. Choose another path with `-o`:

```console
buildprof -o clean-build.pftrace -- cargo build --release
```

Open the resulting file at [buildprof.lalitm.com](https://buildprof.lalitm.com).
The UI processes recordings locally in your browser and does not upload them
without your explicit consent. Traces contain command lines and filesystem
paths, so review them before sharing them.

When run from an interactive terminal inside a graphical Linux session,
Buildprof serves the completed trace on localhost and opens it in the UI
automatically:

```console
buildprof -- cargo build --release
```

This starts a local HTTP server on `localhost:9001`; the trace is fetched
directly by your browser and is not uploaded. Stop the server after viewing the
trace if you do not need it anymore. Headless and CI environments leave this
off automatically; use `--no-open` to disable it explicitly in graphical sessions.

An existing trace can be served and opened with the `open` command:

```console
buildprof open clean-build.pftrace
```

Use `--dev-server` to open it in the UI development server at
`http://localhost:10000`, or `--url URL` to select another Buildprof UI:

```console
buildprof open --dev-server clean-build.pftrace
buildprof open clean-build.pftrace --url https://buildprof.example.com
```

### Compiler details

Compiler-internal tracing is opt-in:

```console
buildprof --compiler-traces -- cargo build
```

This currently imports Clang `-ftime-trace`, explicitly selected LLD
`--time-trace`, and nightly Rust compiler self-profile data. Buildprof injects
profiling flags through compiler wrappers;
this can change compiler cache keys or turn cache hits into misses. Existing
Rust compiler wrappers are kept in the invocation chain, but cache preservation
is not guaranteed when this option is enabled.

## Development

The complete conformance suite runs in the Linux development container:

```console
just bootstrap
just test
```

For a quick host-side check of formatting, lints, unit tests, and the crates.io
package contents:

```console
just release-check
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) and
[AUTHORS](AUTHORS).
