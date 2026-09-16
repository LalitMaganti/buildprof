# Build systems

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
- **Buck2**: local actions and artifact flow through an isolated build daemon.

Anything else that runs as a child process is recorded the same way: shell
scripts, code generators, wrapper scripts, and tools launched by the build.
