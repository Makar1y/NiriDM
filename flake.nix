{
  description = "NiriDM - Niri Display Manager";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);

      # Build the package with the nixpkgs of a given system.
      ndmPackage = system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in import ./nix/package.nix { inherit pkgs; src = ./.; };
    in
    {
      # `nix develop`
      devShells = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
            ];
          };
        });

      # `nix build .`
      packages = forAllSystems (system: {
        default = ndmPackage system;
      });

      # `nix install .#nixosModules.niridm` → programs.NiriDM.*
      nixosModules = {
        niridm = { config, lib, pkgs, ... }:
          import ./nix/modules/programs/niridm.nix {
            inherit config lib pkgs;
            ndmPackage = ndmPackage pkgs.system; # [ ]: Need change to pkgs.stdenv.hostPlatform.system due to deprication?
          };
        default = self.nixosModules.niridm;
      };

      # `nix install .#homeManagerModules.niridm` → programs.NiriDM.*
      homeManagerModules = {
        niridm = { config, lib, pkgs, ... }:
          import ./nix/modules/programs/niridm-home-manager.nix {
            inherit config lib pkgs;
            ndmPackage = ndmPackage pkgs.system; # [ ]: Need change to pkgs.stdenv.hostPlatform.system due to deprication?
          };
        default = self.homeManagerModules.niridm;
      };
    };
}