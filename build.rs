fn main() {
    let tunnel = enabled("CARGO_FEATURE_QUICK_TUNNEL") || enabled("CARGO_FEATURE_NAMED_TUNNEL");
    let edge = enabled("CARGO_FEATURE_QUIC_EDGE") || enabled("CARGO_FEATURE_H2_EDGE");
    for name in ["any_tunnel", "any_edge", "edge_conn", "quic_any", "h2_any"] {
        println!("cargo:rustc-check-cfg=cfg({name})");
    }
    if tunnel {
        println!("cargo:rustc-cfg=any_tunnel");
    }
    if edge {
        println!("cargo:rustc-cfg=any_edge");
    }
    if tunnel && edge {
        println!("cargo:rustc-cfg=edge_conn");
    }
    if enabled("CARGO_FEATURE_QUIC_EDGE") && tunnel {
        println!("cargo:rustc-cfg=quic_any");
    }
    if enabled("CARGO_FEATURE_H2_EDGE") && tunnel {
        println!("cargo:rustc-cfg=h2_any");
    }
}

fn enabled(name: &str) -> bool {
    std::env::var(name).is_ok()
}
