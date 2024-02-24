use capnpc::CompilerCommand;

fn main() {
    println!("cargo:rerun-if-changed=cloudflared/quic/schema/quic_metadata_protocol.capnp");
    println!("cargo:rerun-if-changed=cloudflared/tunnelrpc/go.capnp");
    println!("cargo:rerun-if-changed=cloudflared/tunnelrpc/tunnelrpc.capnp");
    CompilerCommand::new()
        .import_path("cloudflared/tunnelrpc")
        .src_prefix("cloudflared/quic/schema")
        .file("cloudflared/quic/schema/quic_metadata_protocol.capnp")
        .src_prefix("cloudflared/tunnelrpc")
        .file("cloudflared/tunnelrpc/tunnelrpc.capnp")
        .output_path("src")
        .run()
        .expect("failed to compile capnp schema");
}
