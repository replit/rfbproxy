{ pkgs ? import <nixpkgs>{}, versionArg ? "" } :
let
    inherit(pkgs)
        stdenv
        openssl
        libpulseaudio
        pkg-config
        protobuf
        lame
        libopus
        git
        runCommand
        copyPathToStore;

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
                "lame-sys" = attrs: {
                    nativeBuildInputs = [ lame ];
                    buildInputs = [ libpulseaudio ];
                };
                rfbproxy = attrs: {
                    buildInputs = [ openssl protobuf ];
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
    version = builtins.readFile revision;

    src = crate2nix;

    installPhase = ''
        cp -r ${crate2nix} $out
    '';
}
