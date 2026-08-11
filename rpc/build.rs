fn main() {
    println!("cargo:rerun-if-changed=schemas");
    for schema in [
        "rpc.capnp",
        "tunnelrpc.capnp",
        "quic_metadata_protocol.capnp",
    ] {
        let request = capnpc_embedded::CompileCommand::new()
            .file(format!("schemas/{schema}"))
            .src_prefix("schemas")
            .import_path("schemas")
            .compile()
            .expect("failed to compile capnp schema");
        capnpc::codegen::CodeGenerationCommand::new()
            .output_directory(std::env::var("OUT_DIR").unwrap())
            .run(&request[..])
            .expect("failed to generate code");
    }
}
