# Root policy

To record a build on macOS, Buildprof needs root for a few milliseconds. It
uses that time to turn on kernel tracing, then gives root up before your
build starts. Your build, and everything it runs, runs as you.

Linux does not need root. On Linux, Buildprof uses `ptrace` instead, which
works on your own processes.

## Why Buildprof needs root

Buildprof watches your build through the kernel trace facility, the same one
Instruments and `fs_usage` use. macOS only lets root turn it on. Once it is
on, the process that turned it on can keep reading from it after it gives up
root. That is what lets Buildprof drop root so early.

## What happens when you record

1. You run Buildprof as yourself. It saves your environment to a private file
   in your temporary directory, so your build sees the same variables later.
2. Buildprof runs itself again through `sudo`. In a terminal, `sudo` asks for
   your password.
3. As root, Buildprof does four things:
   - looks up which user started it
   - turns on kernel tracing
   - lists the threads already running, so it can tell them apart later
   - reads the clock, to line up the trace's timestamps
4. Buildprof drops root. It switches to your user and your groups, then
   checks that it cannot get root back. If it can, it stops.
5. From here on, Buildprof runs as you. It reads your saved environment,
   starts your build, reads the trace and writes the recording. You own the
   recording file.

## Run it as yourself

Like Homebrew, Buildprof runs as you and asks `sudo` for root when it needs
it. Running it as yourself lets it save your environment before `sudo` resets
it, and gives it a user to run your build as. If you run Buildprof with
`sudo`, or from a root shell, it stops and asks you to run it as yourself.

If you would rather type your password into `sudo` directly, run `sudo -v`
first. Buildprof then uses the login `sudo` remembers, and does not ask.

## While you record

`sudo` stays running as root until Buildprof exits, as it does for any
command. It runs no Buildprof code.

Kernel tracing sees every process on the Mac. Buildprof keeps your build's
events and throws the rest away.

When the recording ends, Buildprof turns kernel tracing off. Ctrl-C goes to
your build, and Buildprof still writes the recording once the build exits.
If Buildprof itself is killed, for example with `kill -9`, it has no chance
to turn tracing off. Tracing then stays on until your next recording, which
takes it over and turns it off at the end.

## Scripts and CI

Without a terminal, `sudo` cannot ask for a password, so it must already be
set up to work without one. GitHub's macOS runners already work this way.

## Check it yourself

While a recording runs, `ps -o user,pid,command` shows Buildprof and your
build running as you. The code that drops root is in `prepare` and
`drop_privileges` in [`src/macos.rs`](../src/macos.rs).
