{ lib, rustPlatform, fetchFromGitHub, openssl, stdenv, libpulseaudio, pkg-config, protobuf, lame, libopus, git, runCommand }:

let
    gitSrc = builtins.filterSource
               (path: type: true)
               ./.;
in
rustPlatform.buildRustPackage rec {
  pname = "rfbproxy";
  revision = runCommand "get-rev" {
      nativeBuildInputs = [ git ];
      dummy = builtins.currentTime;
  } "GIT_DIR=${gitSrc}/.git git rev-parse --short HEAD | tr -d '\n' > $out";
  version = builtins.readFile revision;

  src = ./.;

  cargoSha256 = "1djk818q08lqaz97qqp0wxfx34dvq91sjfnwkz3qq61191j1gp8w";

  buildInputs = [ openssl libpulseaudio protobuf lame libopus ];
  nativeBuildInputs = [ pkg-config ];

  # needed for internal protobuf c wrapper library
  PROTOC = "${protobuf}/bin/protoc";
  PROTOC_INCLUDE = "${protobuf}/include";
}
