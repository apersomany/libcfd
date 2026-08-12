//! TLS configuration for the HTTP/2 edge connection.

use crate::error::Result;
use crate::roots;

/// Builds a `rustls::ClientConfig` trusting the system store plus the
/// Cloudflare origin roots, with an optional user CA appended.
pub(crate) fn tls_client_config(ca_cert_pem: Option<&[u8]>) -> Result<rustls::ClientConfig> {
    let mut store = rustls::RootCertStore::empty();
    for pem in roots::root_pems(ca_cert_pem) {
        for cert in rustls_pki_types::pem::PemObject::pem_slice_iter(&pem).flatten() {
            let _ = store.add(cert);
        }
    }
    Ok(rustls::ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth())
}
