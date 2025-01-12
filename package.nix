{ lib, rustPlatform }:
let
  toml = (lib.importTOML (./Cargo.toml)).package;
in
rustPlatform.buildRustPackage {
  pname = toml.name;
  inherit (toml) version;
  src = ./.;
  cargoLock.lockFile = ./Cargo.lock;

  meta.mainProgram = "linker";
}
