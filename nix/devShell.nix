{ inputs, ... }:
{
  perSystem = { pkgs, system, ... }:
    let
      rustToolchain = inputs.rust-overlay.packages.${system}.rust.override {
        extensions = [ "rustfmt" "clippy" "rust-src" "rust-analyzer" ];
      };
    in
    {
      devShells.default = pkgs.mkShell {
        nativeBuildInputs = [
          rustToolchain
          pkgs.pkg-config
          pkgs.protobuf
          pkgs.cargo-deny
          pkgs.cargo-nextest
          pkgs.cargo-watch
        ];

        buildInputs = [
          pkgs.openssl
          pkgs.zstd
          pkgs.xz
          pkgs.bzip2
        ];

        PROTOC = "${pkgs.protobuf}/bin/protoc";

        shellHook = ''
          echo "niphas devShell ready"
        '';
      };
    };
}
