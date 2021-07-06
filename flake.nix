{
  description = "An RFB proxy that enables WebSockets and audio";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem
      (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          rfbproxy = pkgs.callPackage ./rfbproxy.nix {
            rev = if self ? rev then "0.0.0-${builtins.substring 0 7 self.rev}" else "0.0.0-dirty";
          };
        in
        {
          defaultPackage = rfbproxy;
          packages = {
            inherit rfbproxy;
          };
        }
      );
}
