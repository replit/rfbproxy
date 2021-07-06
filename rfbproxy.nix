{stdenv, openssl, libpulseaudio, protobuf, lame, libopus, git, runCommand, copyPathToStore, rev} :
let
    generatedBuild = import ./Cargo.nix {
        inherit pkgs;
        buildRustCrateForPkgs = pkgs: pkgs.buildRustCrate.override {
            defaultCrateOverrides = pkgs.defaultCrateOverrides // {
                "opus-sys" = attrs: {
                    nativeBuildInputs = [ pkg-config ];
                    buildInputs = [ libopus ];
                };
                "libpulse-sys" = attrs: {
                    nativeBuildInputs = [ pkg-config ];
                    buildInputs = [ libpulseaudio ];
                };
                "libpulse-simple-sys" = attrs: {
                    nativeBuildInputs = [ pkg-config ];
                    buildInputs = [ libpulseaudio ];
                };
                rfbproxy = attrs: {
                    buildInputs = [ openssl protobuf lame ];
                    nativeBuildInputs = [ pkg-config ];

                    # needed for internal protobuf c wrapper library
                    PROTOC = "${protobuf}/bin/protoc";
                    PROTOC_INCLUDE = "${protobuf}/include";
                };
            };
        };
    };

    crate2nix = generatedBuild.rootCrate.build;

in stdenv.mkDerivation {
    pname = "rfbproxy";
    version = rev;

    src = crate2nix;

    installPhase = ''
        cp -r ${crate2nix} $out
    '';
}
