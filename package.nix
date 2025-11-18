{ lib, rustPlatform }:
let
  toml = (lib.importTOML ./Cargo.toml).package;
  fs = lib.fileset;
  s = ./.;
in
rustPlatform.buildRustPackage {
  pname = toml.name;
  inherit (toml) version;
  src = fs.toSource {
    root = s;
    fileset = fs.unions [
      (s + /src)
      (s + /tests)
      (s + /Cargo.lock)
      (s + /Cargo.toml)
    ];
  };
  cargoLock.lockFile = ./Cargo.lock;

  meta.mainProgram = "smfh";
}
