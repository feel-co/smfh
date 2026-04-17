{ lib, rustPlatform }:
let
  toml = (lib.importTOML ./crates/smfh-cli/Cargo.toml).package;
  fs = lib.fileset;
  s = ./.;
in
rustPlatform.buildRustPackage {
  pname = "smfh";
  inherit (toml) version;
  src = fs.toSource {
    root = s;
    fileset = fs.unions [
      (s + /crates)
      (s + /tests)
      (s + /Cargo.lock)
      (s + /Cargo.toml)
    ];
  };
  cargoLock.lockFile = ./Cargo.lock;

  meta.mainProgram = "smfh";
}
