# Releasing Buildprof

A release is one tag push. Everything else is automated, with a few manual
follow-ups for registries that need a pull request.

## One-time setup

Repository secrets:

| Secret | Used by |
| --- | --- |
| `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID` | `deploy-ui.yml`, Pages project `buildprof` |
| `HOMEBREW_TAP_TOKEN` | `release.yml`, pushes the formula to `LalitMaganti/homebrew-tap` |
| `CARGO_REGISTRY_TOKEN` | `deploy-ui.yml`, `cargo publish` after the UI is live |

Also create the empty `homebrew-tap` repository.

## Each release

1. Move the `Unreleased` section of `CHANGELOG.md` under the new version and
   date, and set `version` in `Cargo.toml`. The crate version names the UI's
   `/v<version>/` directory and is what the CLI opens, so they must match.
   Re-record `examples/*.buildprof` with this version if the trace format
   changed (see `examples/README.md`).
2. Run `just release-check`, commit, push, and wait for CI.
3. Tag and push: `git tag v0.2.1 && git push origin v0.2.1`.
4. Review the draft release `Buildprof <version>` that appears on GitHub,
   edit the notes as needed, and publish it.

Then, in order:

- `release.yml` (dist) builds glibc 2.28, musl, and macOS binaries, the shell
  installer, checksums, and the Homebrew formula, and creates the GitHub
  release as a draft. The Homebrew formula points at the release's assets, so
  `brew install` works once the release is published.
- Publishing the release by hand is what fires the next two workflows; a
  release published by automation would not, which is why the draft step is
  not optional.
- `https://buildprof.lalitm.com/install.sh` is a redirect to the latest
  release's installer, written by `assemble-site`, so it needs no update.
- `deploy-ui.yml` builds this release's UI, attaches
  `buildprof-ui-v<version>.tar.zst` to the release, assembles the site from
  every released UI plus `examples/*.buildprof`, deploys it, and only then
  publishes the crate to crates.io.
- `packages.yml` attaches `.deb` and `.rpm` packages built from the release
  tarballs.

Manual follow-ups:

- First release only: submit `packaging/aqua` and `packaging/mise` so
  `mise use -g buildprof` resolves without the `github:` prefix. Both track
  GitHub releases automatically afterwards.
- Later, when there is demand: AUR (`packaging/aur/update <version>`, then
  push to a `buildprof-bin` package) and nixpkgs (`packaging/nix`). Neither is
  part of the initial release.

## Compatibility rules

- Before 1.0 the CLI and UI are in lockstep: the trace format may change
  between releases, and every released UI stays deployed forever under
  `/v<version>/` so any recording still has a UI that reads it. The root
  serves the newest and links to the matching version for other recordings.
- From 1.0 the trace format is stable. Every recording must open in every
  later UI; a change that would break that needs a `buildprof.trace_format`
  bump plus a reader for the old format in the UI, never a new UI for old
  traces.
