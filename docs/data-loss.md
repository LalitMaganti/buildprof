# Data loss

Buildprof cannot always see everything a build does. This page lists what it
can miss, how you can tell, and why it happens. The notes Buildprof prints after
a recording link to the matching section.

| What can be missing | Platform | How you can tell |
|---|---|---|
| [Work done outside the build](#work-done-outside-the-build) | Linux, macOS | A hint after the recording, for known daemons and container tools |
| [Detail inside compilers](#detail-inside-compilers) | Linux, macOS | Not reported |
| [The location of some files](#unknown-paths) | macOS | Marked as unknown in the recording |
| [Events during heavy load](#dropped-events) | macOS | A note after the recording |
| [Arguments of very short commands](#processes-named-after-their-program) | macOS | A note after the recording |
| [A few rarer cases](#not-followed-yet) | macOS | Not reported |

## Work done outside the build

Buildprof records the command you give it and every process that command
starts. Work that runs anywhere else is not recorded. This includes:

- a daemon that was already running, such as Bazel's or Gradle's
- remote execution
- containers started through Docker's daemon

In the recording, this usually looks like one long command with nothing
underneath it. [Troubleshooting](troubleshooting.md) explains how to record
these builds.

## Detail inside compilers

`--compiler-traces` adds what happens inside each compiler. This detail is
missing when:

- Clang is called by an absolute path
- the compiler does not support it, as with stable Rust

See [compiler details](usage.md#compiler-details) for more.

## Unknown paths

Some files appear as `<unknown>/…` rather than at their real location.

**Why.** On macOS, Buildprof learns which files a build opens from a trace that
the kernel writes. When a program opens a file through a relative symbolic
link, the trace records the part of the path after the link. It does not record
the directory that holds the link.

For example, `/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk` is a link to
`MacOSX26.5.sdk`. When the linker reads `libSystem.tbd` from the SDK, the trace
records the path as `MacOSX26.5.sdk/usr/lib/libSystem.tbd`. Nothing in the
trace says that `MacOSX26.5.sdk` is in `/Library/Developer/CommandLineTools/SDKs`.

**What Buildprof does.**

- If another part of the build opens the same file by its full path, at any
  point, Buildprof uses that path.
- Otherwise, Buildprof records the partial path and marks it as unknown. It
  does not try to guess the missing directory, because a wrong guess would
  point to a file that does not exist.

**How often.** Rarely. It almost always affects SDK files, which no step of the
build creates. It therefore does not hide which step produced a file that
another step used.

In a clean `cargo build` of Buildprof, 15 of about 6,900 file reads were
unknown. Thirteen were reads of `SDKSettings.json`, and two were reads of a
link inside a framework.

<!-- buildprof.lalitm.com/diagnose/dropped-events links here; see infra/buildprof.lalitm.com/assemble-site -->
## Dropped events

Some processes, exits or file events may be missing from the recording.

**Why.** The kernel collects events in a fixed-size buffer, and Buildprof
empties it every few milliseconds. If events arrive faster than Buildprof can
read them, the kernel discards the ones that do not fit. Everything running on
the Mac shares this buffer, not just the build.

**How often.** This should not happen with a normal install. In testing, a
build of 10,000 processes lost no events.

If you built Buildprof yourself without `--release`, it reads events about 20
times more slowly and does lose some. Use a release build for recording.

<!-- buildprof.lalitm.com/diagnose/unnamed-processes links here; see infra/buildprof.lalitm.com/assemble-site -->
## Processes named after their program

Some processes are shown with only their program's name, such as `sh`, and no
arguments.

**Why.** The kernel's trace does not include command lines. Instead, Buildprof
reads each command line from the process itself, as soon as the process
starts. If the process finishes before Buildprof reads it, its command line is
gone.

**Launchers.** Some programs are small launchers that start the real program in
the same process. On macOS, `/bin/sh` starts `bash`, and `/usr/bin/cc` starts
`clang`. Buildprof can end up reading a launcher's command line after the real
program has already started.

- If the launcher passed its own arguments on unchanged, as `/bin/sh` does, its
  step keeps them.
- Otherwise its step shows only its name, rather than the other program's
  arguments. Nothing is lost, because the step straight after it, in the same
  process, has that program's own arguments.

Launchers are not counted in the note after the recording.

**How often.** Only very short commands are affected. In a test build that ran
10,001 quick shell and utility commands, two or three lost their arguments. No
compiler, linker or build tool did.

## Not followed yet

Buildprof does not yet handle these cases, and does not report them.

- **Per-thread working directories.** A thread can set its own working
  directory with `pthread_chdir`. Files it then opens by a relative path may be
  recorded with the wrong path, or as unknown.
- **Files opened by `posix_spawn`.** A program that starts a child process can
  ask `posix_spawn` to open files for the child, for example to redirect its
  output. The kernel opens these files itself while it starts the child, so
  they are not recorded.
