# Investigating a slow build

See the [quick start](../README.md#quick-start) to install Buildprof and record
your first build.

Start with a whole-build recording, including any setup or wrapper script you
normally run. This captures downloads, code generation, and other work around
the compiler too. You can also practice navigating with the
[ripgrep example](https://buildprof.lalitm.com/#!/?url=https://buildprof.lalitm.com/examples/ripgrep-release-clean.buildprof)
or follow the [guided tour](ripgrep-tutorial.md) for a worked example.

## Find where the time goes

Time runs left to right, bar width shows duration, and child processes appear
beneath the process which launched them. Use **W / S** to zoom and **A / D**
to pan, or **Ctrl / ⌘ + scroll** to zoom at the pointer.

Look for long-running commands, gaps before compilation starts, and processes
which keep running after everything else finishes. Click a process to inspect
its command line, working directory, lifetime, and exit status. For example,
a long linker invocation is a reason to inspect its LTO flags and then look
inside the linker with [compiler tracing](#compiler-details).

A parent's lifetime includes time spent waiting for children: follow the
process tree down to see which command is still running. Process duration
alone does not tell you whether it is using the CPU or waiting.

## Investigate low parallelism or many small commands

Look at **Build concurrency** above the process tree. It counts live processes
which have no live children at that moment, so a compiler and its waiting
build-system parent do not count twice. Click a low-concurrency interval to
see which processes overlap it, then follow their links to inspect them.
A value of one can still represent a multithreaded compiler using many cores;
this track does not measure CPU utilization.

To summarize a busy interval, click and drag across the **Process tree** track
to select a time range, then open **Build aggregation** in the bottom panel.
Choose **Tool**, **Directory**, or **Action** to group the work. Compare each
group's count, total duration, and longest duration; expand it to inspect
individual processes. This helps distinguish many short commands from a few
long ones.

Aggregation includes processes which never spawned children and whose
lifetimes overlap the selected range. It uses their full lifetimes, including
time outside the selection. Overlapping durations add together, so the total
is neither elapsed build time nor CPU time. To investigate a compiler which
launched a linker, inspect it directly in the process tree.

## Follow inputs back to their producers

To investigate why a command starts late, select it and expand **Produced by**
under **Dependencies**. Each entry identifies a process which wrote a file
the selected command read. Click the process link to jump to its place on
the timeline; **Open as a table…** shows the relationships as a table.
**Consumed by** follows the relationship in the other direction, and
**Show on timeline** displays dependency arrows.

For a linker, this lets you follow object files and archives back to their
compiler or archiver and compare when those producers finished. These are
observed file relationships, not a complete account of the build system's
scheduling decisions; files produced before recording have no producer in
the trace.

![A selected ripgrep compiler process, with callouts for its command and input producers](assets/ripgrep-tour-process.png)

The numbered callouts identify the selected process (1), its command (2),
and the **Produced by** links (3). See the [ripgrep tour](ripgrep-tutorial.md)
to follow these relationships yourself.

## Compiler details

To collect supported compiler events, rerun the build with compiler tracing:

```bash
buildprof --compiler-traces -- cargo +nightly build
```

In the recording, select a process with compiler events and click
**Show compiler track**. Expand its summary track to see per-thread phases;
zoom in and click an event to inspect its duration and arguments.

You can also replay just an expensive compilation or link rather than the
whole build. Run its command from the recorded working directory, keeping
its inputs and flags, with `buildprof --compiler-traces --` in front. The
command really runs again and may overwrite its outputs. For Clang, invoke
`clang` or `clang++` through `PATH`; LLD tracing requires an explicit linker
selection such as `-fuse-ld=lld` on that Clang command. A direct `ld.lld`
invocation is not wrapped. If you rerun the build system instead, make sure
it actually rebuilds the command you want to inspect.

For example, suppose the recorded working directory is `/work/my-project`
and the compilation command is `clang++ -O2 -g -c src/main.cpp -o out/main.o`.
Replay it with:

```bash
cd /work/my-project
buildprof -o compile-detail.buildprof --compiler-traces -- \
  clang++ -O2 -g -c src/main.cpp -o out/main.o
```

For a link originally performed by `clang++ -fuse-ld=lld out/main.o -o out/app`:

```bash
buildprof -o link-detail.buildprof --compiler-traces -- \
  clang++ -fuse-ld=lld out/main.o -o out/app
```

These illustrate the command shape: substitute your recorded directory,
inputs, and full argument list, including libraries and any LTO flags. Keep
response files referenced by `@file` available too. Both `clang` and `clang++`
must be on `PATH` for Buildprof to install its Clang wrappers.

See [compiler support and limitations](../README.md#compiler-details) for
supported compilers and the effects on compiler caches.

## Check whether a change helped

Save separate recordings with `-o before.buildprof` and `-o after.buildprof`,
then open each to compare the whole build and the commands you investigated.
Keep the build command, machine, and cache conditions comparable. A clean
build, an up-to-date rebuild, and a rebuild after editing one file answer
different questions; whether dependencies are already downloaded matters too.
