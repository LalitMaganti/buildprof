# Example recordings

Every `.buildprof` file here is deployed to
`https://buildprof.lalitm.com/examples/<name>` and opened by the README link,
the homepage gallery, and `buildprof open --example <name>`. The names those
refer to live in `EXAMPLES` in `src/args.rs` and in the homepage overlay.

Requirements, checked by `infra/buildprof.lalitm.com/assemble-site` in CI:

- recorded with the Buildprof version being released (the UI and CLI are in
  lockstep before 1.0), so it is a zstd stream carrying a version attribute;
- under 25 MiB, Cloudflare Pages' per-asset limit; recordings compress 10x
  to 20x, so this fits builds of tens of thousands of processes.

Recordings contain command lines and paths from the machine they were made
on. Review before committing.

## Node.js no-op builds

Evidence for the gyp-next and Node.js fixes that stop no-op builds of Node.js
from re-running work. Each is a no-op `make -j12` of Node.js `main` (f88a1d25e)
on 12-core Linux, recorded after a build that had just finished. They were
recorded with `--no-file-events` and recompressed at zstd level 19 to keep them
small, so they show processes but not file accesses:

- `node-noop-main.buildprof`: unpatched, 3,002 processes and 181 compiler and
  linker runs;
- `node-noop-gyp-fix-only.buildprof`: with the gyp-next fix in `tools/gyp`,
  62 processes, still running torque;
- `node-noop-both-fixes.buildprof`: with the torque outputs fix as well,
  55 processes and nothing rebuilt.

These are not listed by `buildprof examples`.
