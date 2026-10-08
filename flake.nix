{
  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1"; # unstable Nixpkgs
    fenix = {
      url = "https://flakehub.com/f/nix-community/fenix/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
    }:
    let
      inherit (nixpkgs) lib;
      validationSource = lib.cleanSourceWith {
        src = self;
        filter =
          path: type:
          let
            relative = lib.removePrefix "${self}/" (toString path);
            inSourceTree = lib.any (root: relative == root || lib.hasPrefix "${root}/" relative) [
              "crates/libcfd"
              "crates/libcfd-rpc"
            ];
            safePath = lib.all (
              part:
              !(lib.hasPrefix "." part)
              && !(builtins.elem part [
                "state"
                "target"
                "research"
              ])
            ) (lib.splitString "/" relative);
          in
          if type == "directory" then
            toString path == toString self || relative == "crates" || (inSourceTree && safePath)
          else
            type == "regular"
            && (
              builtins.elem relative [
                "Cargo.toml"
                "Cargo.lock"
              ]
              || (
                inSourceTree
                && safePath
                && (
                  lib.hasSuffix ".rs" relative
                  || lib.hasSuffix ".capnp" relative
                  || builtins.baseNameOf relative == "Cargo.toml"
                  || relative == "crates/libcfd/src/edge/transport/quic/cloudflare_origin_ca.pem"
                )
              )
            );
      };
      perSystem =
        lib.genAttrs
          [
            "x86_64-linux"
            "aarch64-linux"
            "aarch64-darwin"
          ]
          (
            system:
            let
              pkgs = nixpkgs.legacyPackages.${system};
              rustToolchain = fenix.packages.${system}.stable.withComponents [
                "clippy"
                "rustc"
                "cargo"
                "rustfmt"
                "rust-src"
              ];
              rustPlatform = pkgs.makeRustPlatform {
                cargo = rustToolchain;
                rustc = rustToolchain;
              };
              nativeBuildInputs = with pkgs; [
                rustToolchain
                pkg-config
                cmake
                go
                libclang
                capnproto
                gitMinimal
              ];
              buildInputs = [ pkgs.openssl ] ++ lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.glibc.dev;
              rustTarget = lib.replaceStrings [ "-" ] [ "_" ] pkgs.stdenv.hostPlatform.rust.rustcTarget;
              buildEnvironment = {
                LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
              }
              // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
                BINDGEN_EXTRA_CLANG_ARGS = "-I${pkgs.glibc.dev}/include";
                # BoringSSL's C++ include_next breaks with the GCC wrapper's -isystem ordering.
                "CC_${rustTarget}" = "${pkgs.gcc.cc}/bin/gcc";
                "CXX_${rustTarget}" = "${pkgs.gcc.cc}/bin/g++";
                CFLAGS = "-B${pkgs.glibc}/lib/ -L${pkgs.glibc}/lib -L${pkgs.gcc.cc.lib}/lib";
                CXXFLAGS = "-B${pkgs.glibc}/lib/ -L${pkgs.glibc}/lib -L${pkgs.gcc.cc.lib}/lib";
              };
            in
            {
              checks = {
                secret-hygiene =
                  pkgs.runCommand "libcfd-secret-hygiene"
                    {
                      nativeBuildInputs = [
                        pkgs.gitMinimal
                        pkgs.gitleaks
                      ];
                    }
                    ''
                      cp ${./.gitignore} .gitignore
                      git init --quiet
                      git check-ignore --quiet tests/state/probe
                      # RFC 6455 nonce and synthetic b"top-secret" test credentials only.
                      cat > "$TMPDIR/gitleaks.toml" <<'EOF'
                      [extend]
                      useDefault = true
                      [[allowlists]]
                      description = "Public protocol example and synthetic test credentials"
                      regexTarget = "secret"
                      regexes = ['^dGhlIHNhbXBsZSBub25jZQ==$', '^dG9wLXNlY3JldA==$']
                      EOF
                      gitleaks dir ${validationSource} --config "$TMPDIR/gitleaks.toml" --redact --no-banner
                      touch "$out"
                    '';

                validation = rustPlatform.buildRustPackage {
                  pname = "libcfd-validation";
                  version = "0.2.0";
                  src = validationSource;
                  cargoLock.lockFile = ./Cargo.lock;
                  inherit nativeBuildInputs buildInputs;
                  env = buildEnvironment // {
                    CARGO_NET_OFFLINE = "true";
                    CARGO_INCREMENTAL = "0";
                    CARGO_PROFILE_DEV_DEBUG = "0";
                    CARGO_PROFILE_TEST_DEBUG = "0";
                  };
                  auditable = false;
                  dontUseCmakeConfigure = true;
                  buildPhase = ''
                    runHook preBuild
                    export CARGO_BUILD_JOBS="$NIX_BUILD_CORES"
                    cargo fmt --all --check
                    cargo check --workspace --all-targets --all-features
                    cargo clippy --workspace --all-targets --all-features -- -D warnings
                    cargo test --workspace --all-features
                    cargo check --workspace --all-targets
                    runHook postBuild
                  '';
                  doCheck = false;
                  installPhase = ''
                    mkdir -p "$out"
                    touch "$out/passed"
                  '';
                };
              };

              devShells.default = pkgs.mkShell {
                inherit nativeBuildInputs buildInputs;
                packages = with pkgs; [
                  cargo-deny
                  cargo-edit
                  cargo-watch
                  rust-analyzer
                  nixfmt
                ];
                env = buildEnvironment // {
                  RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
                };
              };

              formatter = pkgs.nixfmt;
            }
          );
    in
    {
      checks = lib.mapAttrs (_: config: config.checks) perSystem;
      devShells = lib.mapAttrs (_: config: config.devShells) perSystem;
      formatter = lib.mapAttrs (_: config: config.formatter) perSystem;
    };
}
