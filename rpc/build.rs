fn main() {
    println!("cargo:rerun-if-changed=schemas");
    for schema in [
        "rpc.capnp",
        "tunnelrpc.capnp",
        "quic_metadata_protocol.capnp",
    ] {
        capnpc::CompilerCommand::new()
            .src_prefix("schemas")
            .import_path("schemas")
            .file(format!("schemas/{schema}"))
            .run()
            .expect("failed to compile capnp schema");
    }
}
