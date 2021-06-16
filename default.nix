{ lib, rustPlatform, fetchFromGitHub, openssl, stdenv, libpulseaudio, pkg-config, protobuf, lame, libopus }:

rustPlatform.buildRustPackage rec {
  pname = "rfbproxy";
  version = "0.1.1-90b68e1";

  src = fetchFromGitHub {
    owner = "replit";
    repo = "rfbproxy";
    rev = "v${version}";
    sha256 = "0v33xmqv7fcaq1spiq7mvqpsx47r6ghgsbv4yhcghalxmy7cb2gc";
  };

  cargoSha256 = "1djk818q08lqaz97qqp0wxfx34dvq91sjfnwkz3qq61191j1gp8w";

  buildInputs = [ openssl libpulseaudio protobuf lame libopus ];
  nativeBuildInputs = [ pkg-config ];

  # needed for internal protobuf c wrapper library
  PROTOC = "${protobuf}/bin/protoc";
  PROTOC_INCLUDE = "${protobuf}/include";
}
