{
  description = "A Nix-flake-based Rust development environment";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1"; # unstable Nixpkgs
    fenix = {
      url = "https://flakehub.com/f/nix-community/fenix/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, ... }@inputs:

    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forEachSupportedSystem =
        f:
        inputs.nixpkgs.lib.genAttrs supportedSystems (
          system:
          f {
            inherit system;
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [
                inputs.self.overlays.default
              ];
            };
          }
        );
    in
    {
      overlays.default = final: prev: {
        rustToolchain =
          with inputs.fenix.packages.${prev.stdenv.hostPlatform.system};
          combine (
            with stable;
            [
              clippy
              rustc
              cargo
              rustfmt
              rust-src
            ]
          );
      };

      devShells = forEachSupportedSystem (
        { pkgs, system }:
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustToolchain
              openssl
              pkg-config
              cmake
              go
              libclang
              capnproto
              glibc.dev
              cargo-deny
              cargo-edit
              cargo-watch
              rust-analyzer
              self.formatter.${system}
            ];

            env = {
              # Required by rust-analyzer
              RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";
              # Required by bindgen (boring-sys)
              LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
              BINDGEN_EXTRA_CLANG_ARGS = "-I${pkgs.glibc.dev}/include";
            };

            shellHook = ''
              # boring-sys builds BoringSSL with cmake, which breaks under the
              # gcc wrapper (its -isystem ordering defeats C++ #include_next).
              # Use the unwrapped compiler and add the crt/libgcc search paths
              # for cmake's try-compile links. Exported here so nix develop's
              # store-path rewriting does not reduce CC/CXX to bare names.
              export CC="${pkgs.gcc.cc}/bin/gcc"
              export CXX="${pkgs.gcc.cc}/bin/g++"
              export CFLAGS="-B${pkgs.glibc}/lib/ -L${pkgs.glibc}/lib -L${pkgs.gcc.cc.lib}/lib"
              export CXXFLAGS="$CFLAGS"
            '';
          };
        }
      );

      formatter = forEachSupportedSystem ({ pkgs, ... }: pkgs.nixfmt);
    };
}
