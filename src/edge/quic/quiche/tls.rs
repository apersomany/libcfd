//! TLS configuration for the QUIC edge connection.
//!
//! quiche 0.29 only ships the BoringSSL backend, so the client context is
//! built with the `boring` crate: verify the peer against the system trust
//! store plus the Cloudflare origin roots that cloudflared bundles, with an
//! optional user CA appended.

use boring::ssl::{SslContextBuilder, SslMethod, SslVerifyMode};
use boring::x509::X509;

use crate::edge::roots;
use crate::error::Result;

/// Builds a `quiche::Config` for a client connection to the edge.
pub(crate) fn client_config(ca_cert_pem: Option<&[u8]>) -> Result<quiche::Config> {
    let mut builder = SslContextBuilder::new(SslMethod::tls_client())?;
    builder.set_verify(SslVerifyMode::PEER);
    {
        let store = builder.cert_store_mut();
        for pem in roots::root_pems(ca_cert_pem) {
            add_pem_certs(store, &pem)?;
        }
    }
    let mut configuration =
        quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, builder)?;
    configuration.verify_peer(true);
    Ok(configuration)
}

fn add_pem_certs(store: &mut boring::x509::store::X509StoreBuilderRef, pem: &[u8]) -> Result<()> {
    for cert in X509::stack_from_pem(pem)? {
        store.add_cert(cert)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_cloudflare_roots_parse() {
        let pem = include_bytes!("../cloudflare_origin_ca.pem");
        let certs = boring::x509::X509::stack_from_pem(pem).expect("bundled roots are valid pem");
        assert_eq!(certs.len(), 3);
    }
}
