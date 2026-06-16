{ self, ... }:
{
  perSystem = { pkgs, ... }:
    let
      docs-src = pkgs.stdenvNoCC.mkDerivation {
        name = "niphas-docs-src";
        src = self;
        phases = [ "unpackPhase" "installPhase" ];
        installPhase = ''
          mkdir -p $out
          # Copy docs directory (resolving symlinks)
          cp -rL docs/. $out/
        '';
      };

      docs-site = pkgs.buildNpmPackage {
        pname = "niphas-docs";
        version = "0.1.0";
        src = docs-src;
        npmDepsHash = "sha256-WGOob022dZ0sEgEtY3WJAeVs04LQ/FlFSpA/9B7MHwU=";
        buildPhase = ''
          npx vitepress build
        '';
        installPhase = ''
          mkdir -p $out
          cp -r .vitepress/dist/* $out/
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
            echo "niphas docs → http://localhost:$PORT"
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
