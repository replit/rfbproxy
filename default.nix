{ pkgs ? import <nixpkgs>{}, versionArg ? "" } :
let
    inherit(pkgs)
        rustPlatform
        openssl
        libpulseaudio
        pkg-config
        protobuf
        lame
        libopus
        git
        runCommand
        copyPathToStore;
in
let
    src = pkgs.copyPathToStore ./.;
    revision = runCommand "get-rev" {
        nativeBuildInputs = [ git ];
        # impure, do every time, see https://github.com/NixOS/nixpkgs/blob/master/pkgs/build-support/fetchgitlocal/default.nix#L9
        dummy = builtins.currentTime;
    } ''
        if [ -d ${src}/.git ]; then
            cd ${src}
            git rev-parse --short HEAD | tr -d '\n' > $out
        else
            echo ${versionArg} | tr -d '\n' > $out
        fi
    '';
in
rustPlatform.buildRustPackage rec {
  pname = "rfbproxy";
  version = builtins.readFile revision;

  inherit src;

  cargoSha256 = "1igdzxq68jd4qmkf76zvwz46iqgrqds4jgfdxirpi0zp3hb8kd0j";

  buildInputs = [ openssl libpulseaudio protobuf lame libopus ];
  nativeBuildInputs = [ pkg-config ];

  # needed for internal protobuf c wrapper library
  PROTOC = "${protobuf}/bin/protoc";
  PROTOC_INCLUDE = "${protobuf}/include";
}
