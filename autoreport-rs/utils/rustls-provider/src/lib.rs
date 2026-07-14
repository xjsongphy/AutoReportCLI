//! Process-wide rustls provider selection, vendored from Codex.

use std::sync::Once;

const REQUIRED_SIGNATURE_SCHEME: rustls::SignatureScheme =
    rustls::SignatureScheme::ECDSA_NISTP521_SHA512;

/// Installs aws-lc-rs once, preserving an already-installed host provider.
pub fn ensure_rustls_crypto_provider() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .is_err()
        {
            return;
        }
        let Some(provider) = rustls::crypto::CryptoProvider::get_default() else {
            panic!("aws-lc-rs rustls crypto provider should be installed");
        };
        assert!(
            provider
                .signature_verification_algorithms
                .supported_schemes()
                .contains(&REQUIRED_SIGNATURE_SCHEME),
            "installed rustls provider must support {REQUIRED_SIGNATURE_SCHEME:?}"
        );
    });
}
