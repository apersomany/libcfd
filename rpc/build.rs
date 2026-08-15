use std::path::{Path, PathBuf};

const SCHEMAS: [&str; 3] = [
    "rpc.capnp",
    "tunnelrpc.capnp",
    "quic_metadata_protocol.capnp",
];

fn generated_names() -> [String; 3] {
    [
        "rpc_capnp.rs",
        "tunnelrpc_capnp.rs",
        "quic_metadata_protocol_capnp.rs",
    ]
    .map(str::to_owned)
}

fn main() {
    println!("cargo:rerun-if-changed=schemas");
    println!("cargo:rerun-if-changed=pregenerated");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));

    if capnp_compiler_available() {
        for schema in SCHEMAS {
            capnpc::CompilerCommand::new()
                .src_prefix("schemas")
                .import_path("schemas")
                .file(format!("schemas/{schema}"))
                .run()
                .expect("failed to compile capnp schema");
        }
        if std::env::var_os("LIBCFD_UPDATE_PREGENERATED").is_some() {
            for file in generated_names() {
                std::fs::copy(out_dir.join(&file), Path::new("pregenerated").join(&file))
                    .expect("failed to refresh pregenerated file");
            }
        } else {
            check_fresh(&out_dir);
        }
    } else {
        // No capnp compiler: fall back to the committed generated code so
        // consumers never need to install capnproto to build this crate.
        for file in generated_names() {
            std::fs::copy(Path::new("pregenerated").join(&file), out_dir.join(&file)).expect(
                "failed to copy pregenerated file; build once in a checkout with the \
                 `capnp` compiler installed to regenerate rpc/pregenerated",
            );
        }
    }
}

fn capnp_compiler_available() -> bool {
    std::process::Command::new("capnp")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn check_fresh(out_dir: &Path) {
    for file in generated_names() {
        let generated = std::fs::read(out_dir.join(&file)).expect("generated file");
        let committed =
            std::fs::read(Path::new("pregenerated").join(&file)).expect("pregenerated file");
        if generated != committed {
            println!(
                "cargo:warning={file} differs from rpc/pregenerated; run \
                 `LIBCFD_UPDATE_PREGENERATED=1 cargo build -p libcfd-rpc` and commit the \
                 refreshed files"
            );
        }
    }
}
