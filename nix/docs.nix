{ self, ... }:
{
  perSystem = { pkgs, ... }:
    let
      docs-site = pkgs.stdenvNoCC.mkDerivation {
        name = "niphas-docs";
        src = self;
        nativeBuildInputs = [ pkgs.mdbook ];
        buildPhase = "mdbook build docs";
        installPhase = ''
          mkdir -p $out
          cp -r docs/book/* $out/
        '';
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

        image-niphas-docs = pkgs.dockerTools.buildLayeredImage {
          name = "ghcr.io/fullzer4/niphas-docs";
          tag = "dev";
          contents = [ pkgs.darkhttpd docs-site pkgs.cacert ];
          config = {
            Entrypoint = [ "${pkgs.darkhttpd}/bin/darkhttpd" "${docs-site}" "--port" "8080" ];
            ExposedPorts = { "8080/tcp" = {}; };
          };
        };
      };
    };
}
