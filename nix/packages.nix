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

        nativeBuildInputs = [
          pkgs.pkg-config
          pkgs.protobuf
          pkgs.clang
          pkgs.lld
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
