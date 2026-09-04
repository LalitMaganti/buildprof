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
