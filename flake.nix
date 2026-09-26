{
  description = "Sagate row-polymorphic language and SQL compiler";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "sagate";
          version = "0.1.0";
          src = builtins.path {
            path = ./.;
            name = "sagate-source";
            filter = path: type:
              let base = builtins.baseNameOf path;
              in base != "target" && base != "result" && base != ".git";
          };
          cargoLock.lockFile = ./Cargo.lock;
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.rustc pkgs.cargo pkgs.rustfmt pkgs.clippy ];
        };
      });
    };
}
