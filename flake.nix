{
  description = "Deno dev env";

  inputs = {
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        flake-utils.follows = "flake-utils";
      };
    };
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs = inputs @ {
    flake-parts,
    nixpkgs,
    flake-utils,
    crane,
    rust-overlay,
    ...
  }: let
    inherit (nixpkgs.lib) optional concatStringsSep;
    systems = flake-utils.lib.system;
    flake = flake-utils.lib.eachDefaultSystem (system: let
      overlays = [ (import rust-overlay) ];
      pkgs = import nixpkgs {
        inherit system overlays;
      };

      aarch64DarwinExternalCargoCrates = concatStringsSep " " ["cargo-instruments@0.4.8" "cargo-about@0.6.1"];

      llvmPackages = pkgs.llvmPackages_16;

      defaultShellConf = {
        buildInputs = [

          pkgs.clang
          llvmPackages.libclang
          llvmPackages.libcxxClang

          pkgs.openssl
          pkgs.libiconv
          pkgs.libclang
          pkgs.lld
          pkgs.cmake
        ];

        nativeBuildInputs = with pkgs;
          [
            pkg-config
            protobuf
            brotli
            bacon

            cargo-nextest
            cargo-insta
          ]
          ++ optional (system == systems.aarch64-darwin) [
            darwin.apple_sdk.frameworks.QuartzCore
            darwin.apple_sdk.frameworks.Foundation
            darwin.apple_sdk.frameworks.CoreFoundation
            darwin.apple_sdk.frameworks.CoreServices
            darwin.apple_sdk.frameworks.Security
            darwin.apple_sdk.frameworks.SystemConfiguration
            darwin.apple_sdk.frameworks.MetalPerformanceShaders
          ]
          ++ optional (system != systems.aarch64-darwin) [
            cargo-about
          ];

        shellHook = ''
          export DYLD_FALLBACK_LIBRARY_PATH=$(rustc --print sysroot)/lib
        '';

        # https://github.com/NixOS/nixpkgs/issues/52447#issuecomment-853429315
        # BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${llvmPackages.libclang.lib}/lib/clang/${lib.getVersion clang}/include";
        LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";

      };
    in {
      devShells.default = pkgs.mkShell defaultShellConf;
    });
  in
    flake-parts.lib.mkFlake {inherit inputs;} {
      inherit flake;

      systems = flake-utils.lib.defaultSystems;

      perSystem = {
        config,
        system,
        ...
      }: {
        _module.args = {
          inherit crane;
          pkgs = import nixpkgs {
            inherit system;
            overlays = [(import rust-overlay)];
          };
        };
      };
    };
}

