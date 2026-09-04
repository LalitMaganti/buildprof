# Contributing to Buildprof

Thanks for helping. Bug reports with a build system and a kernel version are
the most useful thing you can send; fixes and new build-system coverage are
welcome too.

## Layout

- `src/` is the recorder and CLI (Rust). Recording is Linux-only; the trace
  writer and `buildprof open` build everywhere.
- `tests/conformance/` records real Make, CMake, Meson, Cargo, and Go builds
  and checks the resulting traces with Perfetto's Trace Processor.
- `third_party/` pins Perfetto and holds Buildprof's UI as a patch series plus
  permanent overlay files; `tools/perfetto` manages the checkout.
- `infra/` and `.github/` deploy the UI and cut releases; `packaging/` holds
  the files submitted to package registries.

## Building and testing the recorder

Any Linux machine with Rust 1.91 and the build systems the suite covers
(`gcc`, `make`, `cmake`, `ninja`, `meson`, `go`) can run everything:

```bash
uv run dev/in-container-test           # fmt, clippy, unit tests, conformance
uv run dev/in-container-test tests/conformance/test_cli.py -k attributes
```

On macOS, `just bootstrap` builds a Linux development container with the same
toolchain and `just test` runs the suite inside it. `just release-check` runs
the host-side checks that CI runs on macOS.

CI runs the conformance suite on every pull request and fails, rather than
skips, when a build system is missing.

## Working on the UI

```bash
just perfetto-setup        # pinned checkout, patches applied, overlays linked
just perfetto-build-ui     # full build, including the wasm Trace Processor
just perfetto-dev-server   # live-reload server on localhost:10000
buildprof open --dev-server clean-build.buildprof
```

Both commands apply the Buildprof version to the page, so the version
directory and the `VERSION` the UI reports match a release rather than
Perfetto's own version string.

Overlay files under `third_party/overlays/perfetto/` are symlinked into the
checkout, so edit them in place. Changes to upstream Perfetto files are
commits on top of the pin in `third_party/src/perfetto`; run
`just perfetto-capture` to turn them into the patch series. Keep patches
small and prefixed `nopr:` when they only exist for this deployment.

## Commit messages

One line, `area: what changed`, lower case, no trailing period, for example
`linux: explain blocked ptrace and failed exec`. Areas in use include `cli`,
`linux`, `trace`, `ui`, `tests`, `infra`, `build`, and `docs`.

## Releases

See [RELEASING.md](RELEASING.md).
