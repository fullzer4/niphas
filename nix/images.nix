{ inputs, ... }:
{
  perSystem = { pkgs, self', system, ... }:
    let
      mkImage = { name, pkg, contents ? [], entrypoint }:
        pkgs.dockerTools.buildLayeredImage {
          inherit name;
          tag = "dev";
          contents = [
            pkg
            pkgs.cacert
            pkgs.tzdata
          ] ++ contents;
          config = {
            Entrypoint = [ entrypoint ];
            Env = [
              "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              "TZDIR=${pkgs.tzdata}/share/zoneinfo"
            ];
          };
        };
    in
    {
      packages = {
        image-niphas-operator = mkImage {
          name = "ghcr.io/fullzer4/niphas-operator";
          pkg = self'.packages.niphas-operator;
          entrypoint = "/bin/niphas-operator";
        };

        image-niphas-eval = mkImage {
          name = "ghcr.io/fullzer4/niphas-eval";
          pkg = self'.packages.niphas-eval;
          contents = [ pkgs.nix pkgs.git ];
          entrypoint = "/bin/niphas-eval";
        };

        image-niphas-csi = mkImage {
          name = "ghcr.io/fullzer4/niphas-csi";
          pkg = self'.packages.niphas-csi;
          contents = [ pkgs.util-linux ];
          entrypoint = "/bin/niphas-csi";
        };

        image-niphas-runner = pkgs.dockerTools.buildLayeredImage {
          name = "ghcr.io/fullzer4/niphas-runner";
          tag = "dev";
          contents = [
            pkgs.busybox
            pkgs.cacert
          ];
          config = {
            Entrypoint = [ "/bin/sh" ];
            Env = [
              "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
            ];
          };
        };

        image-niphas-mock-eval = mkImage {
          name = "ghcr.io/fullzer4/niphas-mock-eval";
          pkg = self'.packages.niphas-mock-eval;
          entrypoint = "/bin/niphas-mock-eval";
        };
      };
    };
}
