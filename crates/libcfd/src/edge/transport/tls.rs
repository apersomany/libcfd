//! TLS configuration for the HTTP/2 edge connection.

use crate::edge::transport::roots;
use crate::error::Result;

/// Builds a `rustls::ClientConfig` trusting the system store plus the
/// Cloudflare origin roots, with an optional user CA appended.
pub(crate) fn tls_client_config(ca_cert_pem: Option<&[u8]>) -> Result<rustls::ClientConfig> {
    let mut store = rustls::RootCertStore::empty();
    for pem in roots::root_pems(ca_cert_pem) {
        for cert in rustls_pki_types::pem::PemObject::pem_slice_iter(&pem).flatten() {
            let _ = store.add(cert);
        }
    }
    // On systems without /etc/ssl/certs (e.g. macOS, Windows), ensure web PKI roots are available
    #[cfg(feature = "quick-tunnel")]
    {
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    Ok(rustls::ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth())
}
