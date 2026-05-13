{
  overlays.default = final: _: {
    smfh = final.callPackage ./package.nix { };
  };
}
