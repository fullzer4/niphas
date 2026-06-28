{ inputs, ... }:
{
  perSystem = { pkgs, self', system, ... }:
    let
      runtimeLibs = [ pkgs.openssl.out pkgs.zstd.out pkgs.xz.out pkgs.bzip2.out ];
      runtimeLibPath = pkgs.lib.makeLibraryPath runtimeLibs;

      nixConf = pkgs.writeTextDir "etc/nix/nix.conf" ''
        sandbox = false
        experimental-features = nix-command flakes
        accept-flake-config = false
        filter-syscalls = false
      '';


      mkImage = { name, pkg, extraPaths ? [], extraEnv ? [], entrypoint, runAsRoot ? true }:
        pkgs.dockerTools.buildLayeredImage {
          inherit name;
          tag = "dev";
          contents = [ pkg pkgs.cacert pkgs.tzdata ] ++ runtimeLibs ++ extraPaths;
          config = {
            Entrypoint = [ entrypoint ];
            User = if runAsRoot then "0" else "65534";
            Env = [
              "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              "TZDIR=${pkgs.tzdata}/share/zoneinfo"
              "LD_LIBRARY_PATH=${runtimeLibPath}"
            ] ++ extraEnv;
          };
          # Create real /etc/passwd and /etc/group for non-root images.
          # writeTextDir creates symlinks into /nix/store which containerd
          # rejects as "path escapes from parent", so we use fakeRootCommands
          # to write real files directly into the image layer.
          fakeRootCommands = pkgs.lib.optionalString (!runAsRoot) ''
            mkdir -p ./etc
            echo 'nobody:x:65534:65534:Nobody:/tmp:/bin/sh' > ./etc/passwd
            echo 'nogroup:x:65534:' > ./etc/group
          '';
          enableFakechroot = !runAsRoot;
        };
    in
    {
      packages = {
        image-niphas-operator = mkImage {
          name = "ghcr.io/fullzer4/niphas-operator";
          pkg = self'.packages.niphas-operator;
          entrypoint = "/bin/niphas-operator";
          runAsRoot = false;
        };

        image-niphas-eval = mkImage {
          name = "ghcr.io/fullzer4/niphas-eval";
          pkg = self'.packages.niphas-eval;
          extraPaths = [ pkgs.nix pkgs.git nixConf ];
          extraEnv = [ "HOME=/tmp" ];
          entrypoint = "/bin/niphas-eval";
          runAsRoot = false;
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
          contents = [ pkgs.busybox pkgs.cacert ];
          config = {
            Entrypoint = [ "/bin/sh" ];
            Env = [ "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt" ];
          };
        };

        all-images = pkgs.linkFarm "niphas-all-images" [
          { name = "operator.tar.gz"; path = self'.packages.image-niphas-operator; }
          { name = "eval.tar.gz"; path = self'.packages.image-niphas-eval; }
          { name = "csi.tar.gz"; path = self'.packages.image-niphas-csi; }
          { name = "runner.tar.gz"; path = self'.packages.image-niphas-runner; }
        ];
      };
    };
}
