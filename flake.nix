# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

{
  description = "Records every process and file access in a build and shows it as an interactive timeline";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forEachSystem = nixpkgs.lib.genAttrs systems;
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);

      buildprofFor = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = cargoToml.package.name;
        version = cargoToml.package.version;

        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./AUTHORS
            ./CHANGELOG.md
            ./Cargo.lock
            ./Cargo.toml
            ./LICENSE
            ./README.md
            ./src
          ];
        };

        cargoLock = {
          lockFile = ./Cargo.lock;
        };

        # The conformance suite needs Linux build systems and ptrace; unit tests
        # cover the CLI and the trace writer.
        cargoTestFlags = [ "--bins" ];

        meta = {
          description = cargoToml.package.description;
          homepage = cargoToml.package.homepage;
          changelog = "${cargoToml.package.repository}/blob/v${cargoToml.package.version}/CHANGELOG.md";
          license = pkgs.lib.licenses.asl20;
          mainProgram = "buildprof";
          # Recording is Linux-only; the macOS build only views traces.
          platforms = pkgs.lib.platforms.linux ++ pkgs.lib.platforms.darwin;
        };
      };
    in
    {
      packages = forEachSystem (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          buildprof = buildprofFor pkgs;
        in
        {
          inherit buildprof;
          default = buildprof;
        });

      devShells = forEachSystem (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.default ];

            packages = with pkgs; [
              # Rust tooling
              rustc
              cargo
              rustfmt
              clippy
              rust-analyzer
              pkg-config

              # Project tooling
              just
              uv
              python3
            ];

            env = {
              RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
            };
          };
        });

      overlays.default = final: prev: {
        buildprof = buildprofFor final;
      };
    };
}
