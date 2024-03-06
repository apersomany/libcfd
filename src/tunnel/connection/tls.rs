use std::sync::Arc;

use anyhow::Result;
use rustls::{Certificate, ClientConfig, RootCertStore};

pub fn client_config() -> Result<Arc<ClientConfig>> {
    let mut root_cert_store = RootCertStore::empty();
    root_cert_store.add(&Certificate(include_bytes!("cloudflare_ca.der").to_vec()))?;
    Ok(Arc::new(
        ClientConfig::builder()
            .with_safe_defaults()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth(),
    ))
}
