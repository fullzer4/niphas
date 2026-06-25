{ inputs, self, ... }:
{
  perSystem = { pkgs, system, ... }:
    let
      rustToolchain = inputs.rust-overlay.packages.${system}.rust;
      craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;

      protoFilter = path: _type: builtins.match ".*\.proto$" path != null;
      srcFilter = path: type:
        (protoFilter path type) || (craneLib.filterCargoSources path type);

      src = pkgs.lib.cleanSourceWith {
        src = self;
        filter = srcFilter;
      };

      commonArgs = {
        inherit src;
        pname = "niphas";
        strictDeps = true;
        nativeBuildInputs = [ pkgs.pkg-config pkgs.protobuf pkgs.clang pkgs.mold ];
        buildInputs = [ pkgs.openssl pkgs.zstd pkgs.xz pkgs.bzip2 ];
        PROTOC = "${pkgs.protobuf}/bin/protoc";
        CARGO_BUILD_RUSTFLAGS = "-C linker=clang -C link-arg=-fuse-ld=mold --cfg tokio_unstable";
      };

      cargoArtifacts = craneLib.buildDepsOnly (commonArgs // { CARGO_PROFILE = "ci"; });

      mkBin = name: craneLib.buildPackage (commonArgs // {
        inherit cargoArtifacts;
        cargoExtraArgs = "--bin ${name}";
        CARGO_PROFILE = "ci";
        doCheck = false;
      });
    in
    {
      packages = {
        default = craneLib.buildPackage (commonArgs // { inherit cargoArtifacts; });
        niphas-operator = mkBin "niphas-operator";
        niphas-eval = mkBin "niphas-eval";
        niphas-csi = mkBin "niphas-csi";
        niphas-crd-gen = mkBin "niphas-crd-gen";
      };

      checks = {
        fmt = craneLib.cargoFmt { inherit src; };
        clippy = craneLib.cargoClippy (commonArgs // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--workspace --all-targets -- --deny warnings";
        });
        deny = craneLib.cargoDeny { inherit src; };
        nextest = craneLib.cargoNextest (commonArgs // {
          inherit cargoArtifacts;
          partitions = 1;
          partitionType = "count";
          cargoNextestExtraArgs = "--workspace --exclude niphas-e2e";
        });
        eval-http = craneLib.cargoNextest (commonArgs // {
          inherit cargoArtifacts;
          pnameSuffix = "-eval-http";
          cargoNextestExtraArgs = "-p niphas-eval --test '*'";
        });
        csi-grpc = craneLib.cargoNextest (commonArgs // {
          inherit cargoArtifacts;
          pnameSuffix = "-csi-grpc";
          cargoNextestExtraArgs = "-p niphas-csi --test '*'";
        });
      };
    };
}
