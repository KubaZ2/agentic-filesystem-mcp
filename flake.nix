{
  description = "Rust env";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, rust-overlay, ... }:
  let
    supportedArch = [
      "x86_64-linux"
      "aarch64-linux"
      "x86_64-darwin"
      "aarch64-darwin"
    ];

    forAllArch = f: nixpkgs.lib.genAttrs supportedArch (arch:
      let
        overlays = [
          (import rust-overlay)
        ];
        pkgs = import nixpkgs {
          system = arch;
          inherit overlays;
        };
        toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      f { inherit pkgs toolchain; }
    );

    cargoToml = fromTOML (builtins.readFile ./Cargo.toml);
  in
  {
    devShells = forAllArch ({ pkgs, toolchain }: {
      default = pkgs.mkShell {
        packages = [ toolchain ];
      };
    });

    packages = forAllArch ({ pkgs, toolchain }:
      let
        rustPlatform = pkgs.makeRustPlatform {
          cargo = toolchain;
          rustc = toolchain;
        };
      in
      {
        default = rustPlatform.buildRustPackage {
          name = cargoToml.package.name;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
        };
      }
    );
  };
}
