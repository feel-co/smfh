{

  inputs = {
    nixpkgs = {
      type = "github";
      owner = "NixOS";
      repo = "nixpkgs";
      ref = "nixos-unstable";
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
      systems,
    }:
    let
      eachSystem = nixpkgs.lib.genAttrs (import systems);
    in
    {
      formatter = eachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        pkgs.writeShellApplication {
          name = "format";
          runtimeInputs = builtins.attrValues {
            inherit (pkgs) nixfmt-rfc-style fd rustfmt;
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
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.default ];
            packages = builtins.attrValues {
              inherit (pkgs) rust-analyzer clippy rustfmt;
            };
          };
        }
      );
    };
}
