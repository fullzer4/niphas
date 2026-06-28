{ self, ... }:
{
  perSystem = { pkgs, ... }:
    let
      docs-site = pkgs.stdenvNoCC.mkDerivation {
        name = "niphas-docs";
        src = self;
        nativeBuildInputs = [ pkgs.mdbook ];
        buildPhase = "mdbook build docs";
        installPhase = "mkdir -p $out && cp -r docs/book/* $out/";
      };
    in
    {
      packages = {
        niphas-docs = pkgs.writeShellApplication {
          name = "niphas-docs";
          runtimeInputs = [ pkgs.darkhttpd ];
          text = ''
            PORT="''${PORT:-8080}"
            echo "niphas docs: http://localhost:$PORT"
            exec darkhttpd ${docs-site} --port "$PORT"
          '';
        };
        niphas-docs-site = docs-site;
      };
    };
}
