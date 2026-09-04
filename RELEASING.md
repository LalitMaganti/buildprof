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

Also create the empty `homebrew-tap` repository, and publish the example
recordings once with `infra/buildprof.lalitm.com/publish-examples` (they need
to be recorded with the version being released, or newer, so they carry a
version attribute).

## Each release

1. Move the `Unreleased` section of `CHANGELOG.md` under the new version and
   date, and set `version` in `Cargo.toml`. The crate version names the UI's
   `/v<version>/` directory and is what the CLI opens, so they must match.
2. Run `just release-check`, commit, push, and wait for CI.
3. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`.

Then, in order:

- `release.yml` (dist) builds glibc 2.28, musl, and macOS binaries, the shell
  installer, checksums, and the Homebrew formula, and publishes the GitHub
  release.
- `https://buildprof.lalitm.com/install.sh` is a redirect to the latest
  release's installer, written by `assemble-site`, so it needs no update.
- `deploy-ui.yml` builds this release's UI, attaches
  `buildprof-ui-v<version>.tar.zst` to the release, assembles the site from
  every released UI and the examples, deploys it, and only then publishes the
  crate to crates.io.
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

- Every released UI stays deployed forever under `/v<version>/`. The root
  serves the newest and shows a link to the matching version when a trace was
  recorded by a different release.
- `buildprof.trace_format` in the trace only changes when the UI needs to read
  traces differently; a bump is a `Changed` entry in the changelog.
