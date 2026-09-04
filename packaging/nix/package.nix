# nixpkgs derivation for Buildprof; submit to pkgs/by-name/bu/buildprof/.
# Replace the two hashes with the values nix reports for the release.
{
  lib,
  rustPlatform,
  fetchFromGitHub,
  stdenv,
}:

rustPlatform.buildRustPackage (finalAttrs: {
  pname = "buildprof";
  version = "0.2.0";

  src = fetchFromGitHub {
    owner = "LalitMaganti";
    repo = "buildprof";
    tag = "v${finalAttrs.version}";
    hash = lib.fakeHash;
  };

  cargoHash = lib.fakeHash;

  # The conformance suite needs Linux build systems and ptrace; unit tests
  # cover the CLI and the trace writer.
  cargoTestFlags = [ "--bins" ];

  meta = {
    description = "Records every process and file access in a build and shows it as an interactive timeline";
    homepage = "https://buildprof.lalitm.com";
    changelog = "https://github.com/LalitMaganti/buildprof/blob/v${finalAttrs.version}/CHANGELOG.md";
    license = lib.licenses.asl20;
    mainProgram = "buildprof";
    # Recording is Linux-only; the macOS build only views traces.
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
    maintainers = [ ];
  };
})
