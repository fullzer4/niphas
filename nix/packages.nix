{ inputs, ... }:
{
  perSystem = { pkgs, system, ... }:
    let
      rustToolchain = inputs.rust-overlay.packages.${system}.rust;
      craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;

      src = craneLib.cleanCargoSource (craneLib.path ../..);

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
    in
    {
      packages = {
        default = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
        });
      };
    };
}
