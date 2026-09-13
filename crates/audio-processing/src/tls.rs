use base64::Engine as _;
use gio::prelude::*;
use std::io::Write;
use std::sync::OnceLock;

// Called once during shared playback/analysis initialization.
pub(super) fn configure_native_trust() -> Result<(), Box<dyn std::error::Error>> {
    // GnuTLS may reread the anchors when creating connection credentials.
    static ANCHORS: OnceLock<tempfile::NamedTempFile> = OnceLock::new();
    let roots = rustls_native_certs::load_native_certs();
    for error in &roots.errors {
        tracing::warn!(%error, "Could not load a native TLS certificate");
    }
    if roots.certs.is_empty() && !roots.errors.is_empty() {
        return Err("Could not read the native TLS trust store".into());
    }
    let mut anchors = tempfile::NamedTempFile::new()?;
    for certificate in roots.certs {
        writeln!(anchors, "-----BEGIN CERTIFICATE-----")?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(certificate.as_ref());
        for line in encoded.as_bytes().chunks(64) {
            anchors.write_all(line)?;
            anchors.write_all(b"\n")?;
        }
        writeln!(anchors, "-----END CERTIFICATE-----")?;
    }
    let database = gio::TlsFileDatabase::new(anchors.path())?;
    gio::TlsBackend::default().set_default_database(Some(&database));
    let _ = ANCHORS.set(anchors);
    Ok(())
}
