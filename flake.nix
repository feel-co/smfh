{

  inputs = {
    nixpkgs = {
      type = "github";
      owner = "NixOS";
      repo = "nixpkgs";
      ref = "nixos-unstable";
    };
    rust-overlay = {
      type = "github";
      owner = "oxalica";
      repo = "rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    systems = {
      type = "github";
      owner = "nix-systems";
      repo = "default";
    };

  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      systems,
    }:
    let
      eachSystem = nixpkgs.lib.genAttrs (import systems);
    in
    {
      formatter = eachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system}.extend rust-overlay.overlays.default;
        in
        pkgs.writeShellApplication {
          name = "format";
          runtimeInputs = builtins.attrValues {
            inherit (pkgs) nixfmt-rfc-style fd;
            inherit (pkgs.rust-bin.nightly.latest) rustfmt;
          };
          text = ''
            fd '.*\.nix' . -X nixfmt {} \;
            fd '.*\.rs' . -X rustfmt {} \;
          '';
        }
      );

      packages = eachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          linker = pkgs.callPackage ./package.nix { };
          default = self.packages.${system}.linker;
        }
      );
      devShells = eachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system}.extend rust-overlay.overlays.default;
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.default ];
            packages = builtins.attrValues {
              inherit (pkgs) rust-analyzer clippy;
              inherit (pkgs.rust-bin.nightly.latest) rustfmt;
            };
          };
        }
      );
    };
}
