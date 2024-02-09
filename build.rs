use capnpc::CompilerCommand as CapnpcCompilerCommand;

fn main() {
    CapnpcCompilerCommand::new()
        .import_path("src/generated")
        .output_path("src/generated")
        .src_prefix("src/generated")
        .default_parent_module(Vec::from_iter([String::from("generated")]))
        .file("src/generated/quic_metadata_protocol.capnp")
        .file("src/generated/tunnelrpc.capnp")
        .run()
        .unwrap();
}
