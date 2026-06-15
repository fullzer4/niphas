{ inputs, self, ... }:
{
  perSystem = { pkgs, system, ... }:
    let
      rustToolchain = inputs.rust-overlay.packages.${system}.rust;
      craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;

      src = craneLib.cleanCargoSource self;

      commonArgs = {
        inherit src;
        strictDeps = true;

        nativeBuildInputs = [
          pkgs.pkg-config
          pkgs.protobuf
        ];

        buildInputs = [
          pkgs.openssl
          pkgs.zstd
          pkgs.xz
          pkgs.bzip2
        ];

        PROTOC = "${pkgs.protobuf}/bin/protoc";
      };

      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      mkBin = name: craneLib.buildPackage (commonArgs // {
        inherit cargoArtifacts;
        cargoExtraArgs = "--bin ${name}";
        doCheck = false;
      });
    in
    {
      packages = {
        default = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
        });

        niphas-operator = mkBin "niphas-operator";
        niphas-eval = mkBin "niphas-eval";
        niphas-csi = mkBin "niphas-csi";
        niphas-crd-gen = mkBin "niphas-crd-gen";
        niphas-mock-eval = mkBin "niphas-mock-eval";
      };
    };
}
