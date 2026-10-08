fn main() {
    let tunnel = enabled("CARGO_FEATURE_QUICK_TUNNEL") || enabled("CARGO_FEATURE_NAMED_TUNNEL");
    let quinn = enabled("CARGO_FEATURE_QUIC_EDGE") || enabled("CARGO_FEATURE_QUIC_EDGE_QUINN");
    let quiche = enabled("CARGO_FEATURE_QUIC_EDGE_QUICHE");
    let quic = quinn || quiche;
    let h2 = enabled("CARGO_FEATURE_H2_EDGE");
    let edge = quic || h2;
    for name in [
        "any_tunnel",
        "any_edge",
        "edge_conn",
        "quic_any",
        "quic_quinn",
        "quic_quiche",
        "h2_any",
    ] {
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
    if quic && tunnel {
        println!("cargo:rustc-cfg=quic_any");
    }
    // The QUIC backends are mutually exclusive; an explicitly requested quiche
    // backend wins over the default quinn backend so `--all-features` and
    // default features plus `quic-edge-quiche` resolve deterministically.
    if quiche {
        println!("cargo:rustc-cfg=quic_quiche");
        if quinn {
            println!(
                "cargo:warning=quic-edge-quiche and quic-edge (quinn) are both enabled; using quiche"
            );
        }
    } else if quinn {
        println!("cargo:rustc-cfg=quic_quinn");
    }
    if h2 && tunnel {
        println!("cargo:rustc-cfg=h2_any");
    }
}

fn enabled(name: &str) -> bool {
    std::env::var(name).is_ok()
}
