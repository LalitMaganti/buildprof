# Packaging

Files for the install channels that live outside this repository. The GitHub
release itself, the shell installer, the Homebrew formula, and the `.deb` and
`.rpm` packages are produced by the release workflows; these are the pieces
someone has to submit elsewhere.

| Directory | Goes to | When |
| --- | --- | --- |
| `aur/` | AUR package `buildprof-bin` | every release, via `aur/update` |
| `nix/` | `pkgs/by-name/bu/buildprof/package.nix` in nixpkgs | first release, then version bumps |
| `aqua/` | `pkgs/github_release/github.com/LalitMaganti/buildprof/` in the aqua registry | first release only |
| `mise/` | `registry.toml` in the mise repository | first release only |

mise users can install without any registry entry:

```bash
mise use -g github:LalitMaganti/buildprof
```
