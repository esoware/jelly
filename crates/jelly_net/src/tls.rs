use std::sync::{Arc, OnceLock};

use jelly_core::{ChainId, Clock, PinStore, SpkiPin};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{VerifierBuilderError, WebPkiServerVerifier};
use rustls::crypto::{CryptoProvider, ring, verify_tls13_signature_with_raw_key};
use rustls::pki_types::{CertificateDer, ServerName, SubjectPublicKeyInfoDer, UnixTime};
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, PeerMisbehaved, RootCertStore,
    SignatureScheme,
};

use crate::error::NetError;
use crate::spki::{public_key_parts, spki_sha256, subject_public_key_info};

const NO_PROTOCOL_VERSION: &str =
    "the ring provider does not support the default protocol versions";

/// Every configuration here names `ring` explicitly, so nothing depends on a process-wide
/// default provider being installed.
fn crypto_provider() -> &'static Arc<CryptoProvider> {
    static CELL: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    CELL.get_or_init(|| Arc::new(ring::default_provider()))
}

/// The compiled-in Mozilla root bundle.
///
/// The platform store isn't used, so a root pushed onto the machine by policy can't
/// intercept wallet traffic.
fn roots() -> &'static Arc<RootCertStore> {
    static CELL: OnceLock<Arc<RootCertStore>> = OnceLock::new();
    CELL.get_or_init(|| Arc::new(RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() }))
}

fn ca_verifier() -> Result<Arc<WebPkiServerVerifier>, NetError> {
    static CELL: OnceLock<Result<Arc<WebPkiServerVerifier>, VerifierBuilderError>> =
        OnceLock::new();
    CELL.get_or_init(|| {
        WebPkiServerVerifier::builder_with_provider(
            Arc::clone(roots()),
            Arc::clone(crypto_provider()),
        )
        .build()
    })
    .clone()
    .map_err(|source| {
        NetError::config("the compiled-in root certificate store is unusable", source)
    })
}

/// Full CA validation against the compiled-in root bundle, for every transport but
/// Electrum, including the shared HTTP client.
pub(crate) fn ca_validated_client_config() -> Result<Arc<ClientConfig>, NetError> {
    static CELL: OnceLock<Result<Arc<ClientConfig>, rustls::Error>> = OnceLock::new();
    CELL.get_or_init(|| {
        let config = ClientConfig::builder_with_provider(Arc::clone(crypto_provider()))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(Arc::clone(roots()))
            .with_no_client_auth();
        Ok(Arc::new(config))
    })
    .clone()
    .map_err(|source| NetError::config(NO_PROTOCOL_VERSION, source))
}

/// Electrum-only TLS verification that pins the SPKI hash on first use.
///
/// The key is pinned even when the certificate is CA-valid, and `verdict` uses CA validity
/// to decide whether a later key change is allowed.
pub(crate) fn pinned_client_config(
    pins: Arc<dyn PinStore>,
    clock: Arc<dyn Clock>,
    chain: ChainId,
) -> Result<Arc<ClientConfig>, NetError> {
    let verifier = TofuVerifier {
        ca: ca_verifier()?,
        provider: Arc::clone(crypto_provider()),
        pins,
        clock,
        chain,
    };
    let config = ClientConfig::builder_with_provider(Arc::clone(crypto_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|source| NetError::config(NO_PROTOCOL_VERSION, source))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// What the pinning verifier does with the key a server just presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict<'a> {
    /// The recorded key was presented again. Nothing to write.
    Known,
    /// The host has never been seen, or a chain to the compiled-in roots vouches for the
    /// new key.
    Record,
    /// The recorded key changed and no authority vouches for the replacement. It carries
    /// the pin so the refusal can name both fingerprints.
    Refused(&'a SpkiPin),
}

/// A changed key is accepted only when its certificate validates against the
/// compiled-in roots, which allows CA-backed renewal. Any other change is refused and
/// never re-pinned.
fn verdict<'a>(
    recorded: Option<&'a SpkiPin>,
    presented: &[u8; 32],
    is_ca_valid: bool,
) -> Verdict<'a> {
    match recorded {
        None => Verdict::Record,
        Some(pin) if pin.sha256() == presented => Verdict::Known,
        Some(_) if is_ca_valid => Verdict::Record,
        Some(pin) => Verdict::Refused(pin),
    }
}

#[derive(Debug)]
struct TofuVerifier {
    ca: Arc<WebPkiServerVerifier>,
    provider: Arc<CryptoProvider>,
    pins: Arc<dyn PinStore>,
    clock: Arc<dyn Clock>,
    chain: ChainId,
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let presented = spki_sha256(end_entity.as_ref()).ok_or(CertificateError::BadEncoding)?;
        let host = server_name.to_str().to_ascii_lowercase();
        let is_ca_valid = self
            .ca
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .is_ok();
        let recorded = self
            .pins
            .get(self.chain, &host)
            .map_err(|_| rustls::Error::General("pin store unavailable".to_owned()))?;

        match verdict(recorded.as_ref(), &presented, is_ca_valid) {
            Verdict::Known => Ok(ServerCertVerified::assertion()),
            Verdict::Record => {
                let first_seen = self.clock.now_secs().unwrap_or_default();
                self.pins
                    .put(self.chain, &host, SpkiPin::new(presented, first_seen))
                    .map_err(|_| rustls::Error::General("pin store unavailable".to_owned()))?;
                Ok(ServerCertVerified::assertion())
            }
            // Public key hashes are safe to log, and a user reporting this needs both to
            // compare.
            Verdict::Refused(pin) => Err(rustls::Error::General(format!(
                "the tls key for {host} changed: pinned {pin}, presented {}",
                hex::encode(presented)
            ))),
        }
    }

    /// The handshake is checked against the certificate's key alone, since that's what the
    /// pin trusts. rustls's own check parses the whole certificate, and its parser refuses the
    /// version 1 certificates some self-signed Electrum servers still present.
    ///
    /// rustls offers a key-only check for TLS 1.3 but not for TLS 1.2, so this is that
    /// check. A TLS 1.2 scheme names no curve and maps to several algorithms, so the one
    /// matching the key's own algorithm checks the signature.
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let (algorithm_id, key) = subject_public_key_info(cert.as_ref())
            .and_then(public_key_parts)
            .ok_or(CertificateError::BadEncoding)?;
        let candidates = self
            .provider
            .signature_verification_algorithms
            .mapping
            .iter()
            .find_map(|&(scheme, candidates)| (scheme == dss.scheme).then_some(candidates))
            .ok_or(PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme)?;
        let Some(algorithm) = candidates
            .iter()
            .find(|candidate| candidate.public_key_alg_id().as_ref() == algorithm_id)
        else {
            let signature_algorithm_id = candidates
                .first()
                .map(|candidate| candidate.signature_alg_id().as_ref().to_vec())
                .unwrap_or_default();
            return Err(CertificateError::UnsupportedSignatureAlgorithmForPublicKeyContext {
                signature_algorithm_id,
                public_key_algorithm_id: algorithm_id.to_vec(),
            }
            .into());
        };
        algorithm
            .verify_signature(key, message, dss.signature())
            .map(|()| HandshakeSignatureValid::assertion())
            .map_err(|_| CertificateError::BadSignature.into())
    }

    /// Checked against the certificate's key alone, for the reason the TLS 1.2 check gives.
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let spki = subject_public_key_info(cert.as_ref()).ok_or(CertificateError::BadEncoding)?;
        verify_tls13_signature_with_raw_key(
            message,
            &SubjectPublicKeyInfoDer::from(spki),
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST: [u8; 32] = [7u8; 32];
    const SECOND: [u8; 32] = [9u8; 32];

    fn pinned(key: [u8; 32]) -> SpkiPin {
        SpkiPin::new(key, 1_700_000_000)
    }

    #[test]
    fn an_unseen_host_is_recorded_however_it_was_signed() {
        assert_eq!(verdict(None, &FIRST, false), Verdict::Record);
        assert_eq!(verdict(None, &FIRST, true), Verdict::Record);
    }

    #[test]
    fn the_recorded_key_needs_no_authority() {
        assert_eq!(verdict(Some(&pinned(FIRST)), &FIRST, false), Verdict::Known);
        assert_eq!(verdict(Some(&pinned(FIRST)), &FIRST, true), Verdict::Known);
    }

    #[test]
    fn a_self_signed_key_that_changed_is_refused_and_never_re_pinned() {
        let recorded = pinned(FIRST);
        assert_eq!(verdict(Some(&recorded), &SECOND, false), Verdict::Refused(&recorded));
    }

    /// A certificate from a public authority is renewed with a fresh key every
    /// few months. Refusing that would take an Electrum server on Let's Encrypt
    /// out of the rotation for good.
    #[test]
    fn a_ca_valid_replacement_is_recorded_over_the_old_key() {
        assert_eq!(verdict(Some(&pinned(FIRST)), &SECOND, true), Verdict::Record);
    }
}
