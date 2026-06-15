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
          pkgs.clang
          pkgs.lld
          pkgs.cargo-deny
          pkgs.cargo-nextest
          pkgs.cargo-watch
          pkgs.kubectl
          pkgs.kubernetes-helm
          pkgs.kind
        ];

        buildInputs = [
          pkgs.openssl
          pkgs.zstd
          pkgs.xz
          pkgs.bzip2
        ];

        PROTOC = "${pkgs.protobuf}/bin/protoc";

        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
          pkgs.openssl
          pkgs.zstd
          pkgs.xz
          pkgs.bzip2
        ];

        shellHook = ''
          echo "niphas devShell ready"
        '';
      };
    };
}
