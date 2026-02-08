{
  lib,
  testers,
  self,
  writeShellScriptBin,
  stdenv,
}:
let
  userHome = "/home/alice";
in
{
  default =
    (testers.runNixOSTest {
      name = "smfh";

      nodes.node1 = {
        users.groups.alice = { };
        users.users.alice = {
          isNormalUser = true;
          home = userHome;
          password = "";
        };

        environment = {
          etc =
            let
              prepend_home = map (
                x:
                x
                // {
                  target = userHome + x.target;
                }
                // (lib.optionalAttrs (x ? source) {
                  source = userHome + x.source;
                })
              );
            in

            {
              "smfh/manifest.json".text = builtins.toJSON {
                files = prepend_home [
                  {
                    type = "symlink";
                    source = "/source/file";
                    target = "/output/symlink";
                  }
                ];
                clobber_by_default = false;
                version = 3;
              };
              "smfh/new_manifest.json".text = builtins.toJSON {
                files = prepend_home [ ];
                clobber_by_default = false;
                version = 3;
              };
            };
          systemPackages = [
            (writeShellScriptBin "setup" ''
              mkdir -p "${userHome}/"{source,output}
              echo 'ooga booga' > '${userHome}/source/file'
            '')

            self.packages.${stdenv.system}.smfh
          ];
        };
      };
      defaults = { };
      testScript = ''
        node1.succeed("su alice -c 'setup'")
        node1.succeed("su alice -c 'smfh -v activate /etc/smfh/manifest.json'")
        node1.succeed("[ -h '${userHome}/output/symlink' ]")
      '';
    }).config.result;
}
