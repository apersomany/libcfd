//! Trust store assembly shared by the QUIC and HTTP/2 edge transports.
//!
//! Mirrors cloudflared's `tlsconfig` behavior: the system trust store plus
//! the bundled Cloudflare origin roots, with a user-supplied CA appended
//! rather than replacing the store.

const SYSTEM_CA_PATHS: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/cert.pem",
];

/// Returns PEM bundles for the system store, the bundled Cloudflare origin
/// roots, and any user-supplied CA, in that order.
pub(crate) fn root_pems(ca_cert_pem: Option<&[u8]>) -> Vec<Vec<u8>> {
    let mut pems = Vec::new();
    let mut found_system = false;
    for path in SYSTEM_CA_PATHS {
        if let Ok(bytes) = std::fs::read(path) {
            pems.push(bytes);
            found_system = true;
            break;
        }
    }
    if !found_system {
        tracing::warn!("no system CA bundle found; using bundled roots only");
    }
    pems.push(include_bytes!("quic/cloudflare_origin_ca.pem").to_vec());
    if let Some(custom) = ca_cert_pem {
        pems.push(custom.to_vec());
    }
    pems
}
