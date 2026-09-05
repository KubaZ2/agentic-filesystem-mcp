{
  description = "Rust env";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { nixpkgs, rust-overlay, ... }:
  let
    supportedArch = [
      "x86_64-linux"
      "aarch64-linux"
      "x86_64-darwin"
      "aarch64-darwin"
    ];

    forAllArch = nixpkgs.lib.genAttrs supportedArch;
  in
  {
    devShells = forAllArch (arch:
      let
        overlays = [
          (import rust-overlay)
        ];
        pkgs = import nixpkgs {
          system = arch;
          inherit overlays;
        };
      in
      {
        default = pkgs.mkShell {
          packages = with pkgs; [
            (rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
          ];
        };
      }
    );
  };
}
