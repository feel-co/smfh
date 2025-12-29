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
            fd "$@" -t f -e nix -X nixfmt '{}'
            fd "$@" -t f -e rs -X rustfmt '{}'
          '';
        }
      );

      packages = eachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          smfh = pkgs.callPackage ./package.nix { };
          default = self.packages.${system}.smfh;
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

      overlays.default = final: _: {
        smfh = final.callPackage ./package.nix { };
      };
    };
}
