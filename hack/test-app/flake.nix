{
  description = "Niphas test application — tiny HTTP server with status page";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";

  outputs = { nixpkgs, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};

      staticSite = pkgs.writeTextDir "index.html" ''
        <!DOCTYPE html>
        <html lang="en">
        <head>
          <meta charset="utf-8">
          <meta name="viewport" content="width=device-width,initial-scale=1">
          <title>niphas</title>
          <style>
            * { margin: 0; padding: 0; box-sizing: border-box; }
            body {
              font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif;
              background: #0a0a0a;
              color: #e0e0e0;
              min-height: 100vh;
              display: flex;
              align-items: center;
              justify-content: center;
            }
            .card {
              background: #151515;
              border: 1px solid #2a2a2a;
              border-radius: 16px;
              padding: 48px;
              max-width: 520px;
              width: 90%;
              text-align: center;
            }
            .logo {
              font-size: 48px;
              font-weight: 700;
              letter-spacing: -2px;
              background: linear-gradient(135deg, #7eb4ea, #a78bfa);
              -webkit-background-clip: text;
              -webkit-text-fill-color: transparent;
              margin-bottom: 8px;
            }
            .sub {
              font-size: 14px;
              color: #666;
              margin-bottom: 32px;
            }
            .status {
              display: flex;
              flex-direction: column;
              gap: 12px;
              text-align: left;
            }
            .row {
              display: flex;
              justify-content: space-between;
              align-items: center;
              padding: 12px 16px;
              background: #1a1a1a;
              border-radius: 8px;
              font-size: 14px;
            }
            .row .label { color: #888; }
            .row .value { font-family: 'SF Mono', monospace; color: #e0e0e0; }
            .badge {
              display: inline-block;
              padding: 2px 10px;
              border-radius: 12px;
              font-size: 12px;
              font-weight: 600;
            }
            .ok { background: #0d3320; color: #34d399; }
            .nix { background: #1e1b4b; color: #a78bfa; }
            .foot {
              margin-top: 32px;
              font-size: 12px;
              color: #444;
            }
          </style>
        </head>
        <body>
          <div class="card">
            <div class="logo">niphas</div>
            <div class="sub">nix-native workload platform for kubernetes</div>
            <div class="status">
              <div class="row">
                <span class="label">status</span>
                <span class="badge ok">running</span>
              </div>
              <div class="row">
                <span class="label">source</span>
                <span class="badge nix">nix flake</span>
              </div>
              <div class="row">
                <span class="label">delivery</span>
                <span class="value">CSI volume mount</span>
              </div>
              <div class="row">
                <span class="label">binary cache</span>
                <span class="value">cache.nixos.org</span>
              </div>
              <div class="row">
                <span class="label">server</span>
                <span class="value">darkhttpd</span>
              </div>
            </div>
            <div class="foot">
              served from /nix/store via niphas CSI driver
            </div>
          </div>
        </body>
        </html>
      '';
    in
    {
      packages.${system}.default = pkgs.writeShellApplication {
        name = "niphas-test-app";
        runtimeInputs = [ pkgs.darkhttpd ];
        text = ''
          echo "niphas-test-app: serving on port ''${PORT:-8080}"
          exec darkhttpd ${staticSite} --port "''${PORT:-8080}" --no-listing
        '';
      };
    };
}
