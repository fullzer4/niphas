{ inputs, ... }:
{
  perSystem = { pkgs, self', system, ... }:
    let
      # Runtime shared libraries needed by all Rust binaries
      runtimeLibs = [
        pkgs.openssl.out
        pkgs.zstd.out
        pkgs.xz.out
        pkgs.bzip2.out
      ];

      mkImage = { name, pkg, extraPaths ? [], entrypoint }:
        pkgs.dockerTools.buildLayeredImage {
          inherit name;
          tag = "dev";
          contents = [
            pkg
            pkgs.cacert
            pkgs.tzdata
          ] ++ runtimeLibs ++ extraPaths;
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
          extraPaths = [ pkgs.nix pkgs.git ];
          entrypoint = "/bin/niphas-eval";
        };

        image-niphas-csi = mkImage {
          name = "ghcr.io/fullzer4/niphas-csi";
          pkg = self'.packages.niphas-csi;
          extraPaths = [ pkgs.util-linux ];
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

        all-images = pkgs.linkFarm "niphas-all-images" [
          { name = "operator.tar.gz"; path = self'.packages.image-niphas-operator; }
          { name = "eval.tar.gz"; path = self'.packages.image-niphas-eval; }
          { name = "csi.tar.gz"; path = self'.packages.image-niphas-csi; }
          { name = "runner.tar.gz"; path = self'.packages.image-niphas-runner; }
          { name = "mock-eval.tar.gz"; path = self'.packages.image-niphas-mock-eval; }
        ];
      };
    };
}
