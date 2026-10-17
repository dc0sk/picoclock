//! NTS-KE client (RFC 8915 section 4): the TLS 1.3 exchange that gives cookies and the two keys. Generic over the
//! socket, so the same code runs on the clock (an embassy-net TCP socket) and in the host tests (tokio).
//!
//! Certificates are checked against the roots the caller passes ([`BUILT_IN_ROOTS`] plus any the user added) and the host name; the dates are checked with the
//! clock the caller supplies and the exchange is refused when that clock has no valid time ([`TlsClock::now`] is `None`),
//! because embedded-tls would then skip the date check silently.

use embedded_io_async::{Read, Write};
use embedded_tls::webpki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CertificateEntryRef, CertificateRef, CertificateVerifyRef, SignatureScheme, TlsCipherSuite, TlsClock, TlsConfig,
    TlsConnection, TlsContext, TlsError, TlsVerifier,
};
use heapless::Vec;

use crate::nts::{self, NtsError, Session};

/// ISRG Root X2 (ECDSA P-384): the root of Let's Encrypt's ECDSA chains, which most public NTS servers use.
/// SHA-256 fingerprint 69:72:9B:8E:15:A8:6E:FC:17:7A:57:AF:B7:17:1D:FC:64:AD:D2:8C:2F:CA:8C:F1:50:7E:34:45:3C:CB:14:70.
const ISRG_ROOT_X2: &[u8] = include_bytes!("../roots/isrg-root-x2.der");
/// SSL.com Root Certification Authority ECC (P-384), used by time.cloudflare.com.
/// SHA-256 fingerprint 34:17:BB:06:CC:60:07:DA:1B:96:1C:92:0B:8A:B4:CE:3F:AD:82:0E:4A:A3:0B:9A:CB:C4:A7:4E:BD:CE:BC:65.
const SSL_COM_ROOT_ECC: &[u8] = include_bytes!("../roots/ssl-com-root-ecc.der");

/// ISRG Root X1 (RSA 4096): the root of Let's Encrypt's RSA chains, which Netnod, PTB and others use. Only built in with the
/// `rsa` feature (the Pico 2 W): verifying an RSA chain needs an allocator.
/// SHA-256 fingerprint 96:BC:EC:06:26:49:76:F3:74:60:77:9A:CF:28:C5:A7:CF:E8:A3:C0:AA:E1:1A:8F:FC:EE:05:C0:BD:DF:08:C6.
#[cfg(feature = "rsa")]
const ISRG_ROOT_X1: &[u8] = include_bytes!("../roots/isrg-root-x1.der");

/// The built-in roots. A server is trusted if its chain ends at any of them (and the host name and dates match). ECDSA chains
/// can always be verified; RSA chains only in builds with the `rsa` feature (Pico 2 W), so on the Pico W a server with an
/// RSA certificate fails with a certificate error.
#[cfg(feature = "rsa")]
pub const BUILT_IN_ROOTS: &[&[u8]] = &[ISRG_ROOT_X2, SSL_COM_ROOT_ECC, ISRG_ROOT_X1];
#[cfg(not(feature = "rsa"))]
pub const BUILT_IN_ROOTS: &[&[u8]] = &[ISRG_ROOT_X2, SSL_COM_ROOT_ECC];

#[derive(Debug)]
pub enum KeError {
    /// The device has no valid time, so certificate dates cannot be checked. Nothing was sent.
    NoTime,
    Tls(TlsError),
    Nts(NtsError),
    /// The server closed the connection before the NTS-KE message was complete.
    Closed,
}

impl From<NtsError> for KeError {
    fn from(e: NtsError) -> Self {
        KeError::Nts(e)
    }
}

/// Size of the server's end-entity certificate we can hold while verifying it.
const CERT_SIZE: usize = 2048;
/// Room for the whole certificate chain the server sends (leaf and intermediates), kept until it is verified.
const CHAIN_BYTES: usize = 6144;
const MAX_CHAIN: usize = 6;
/// Largest `CertificateVerify` signature kept: 512 bytes is an RSA-4096 signature (ECDSA ones are under 110).
const MAX_SIGNATURE: usize = 512;
type Hash = <Aes128GcmSha256 as TlsCipherSuite>::Hash;

/// Verifies the server certificate *after* the key exchange instead of during the TLS handshake.
///
/// Why: checking a P-384 chain takes several seconds on a Cortex-M0+ without hardware crypto (measured on the RP2040: about
/// 4.7 s for Cloudflare's chain), and NTS-KE servers drop a client that is silent for much more than about 2 s in the
/// middle of the handshake. The handshake itself (key agreement, record encryption) is fast.
///
/// Why that is safe here: NTS-KE sends no secret to the server (our request is a fixed public 16-byte offer), and nothing
/// the server answers is used until [`DeferredVerifier::finish`] has run the real certificate and `CertificateVerify`
/// checks on exactly the data the handshake delivered. A man in the middle can complete the handshake with its own
/// certificate, but then `finish` fails and the cookies and keys are thrown away. Do not use this for a protocol in
/// which the client sends secrets (a login, a password) before it knows who the server is.
pub struct DeferredVerifier<'a, C: TlsClock> {
    roots: &'a [&'a [u8]],
    clock: core::marker::PhantomData<C>,
    hostname: Option<heapless::String<64>>,
    transcript: Option<Hash>,
    chain: Vec<u8, CHAIN_BYTES>,
    lens: Vec<u16, MAX_CHAIN>,
    signature: Option<(SignatureScheme, Vec<u8, MAX_SIGNATURE>)>,
}

impl<'a, C: TlsClock> DeferredVerifier<'a, C> {
    pub fn new(roots: &'a [&'a [u8]]) -> Self {
        Self { roots, clock: core::marker::PhantomData, hostname: None, transcript: None, chain: Vec::new(), lens: Vec::new(), signature: None }
    }

    /// Runs the checks that were skipped during the handshake. Fails unless the handshake delivered a chain and a
    /// `CertificateVerify`, so a handshake that never presented a certificate cannot pass. The recorded chain is tried
    /// against each root in turn (no new connection needed); the first root it chains to decides, and if none does the
    /// error of the last attempt is returned.
    pub fn finish(&mut self) -> Result<(), TlsError> {
        let transcript = self.transcript.take().ok_or(TlsError::MissingHandshake)?;
        let (scheme, sig) = self.signature.take().ok_or(TlsError::MissingHandshake)?;
        let mut last = TlsError::InvalidCertificate;
        for root in self.roots {
            let mut cert = CertificateRef::with_context(&[]);
            let mut at = 0;
            for &len in self.lens.iter() {
                let der = self.chain.get(at..at + len as usize).ok_or(TlsError::InternalError)?;
                cert.add(CertificateEntryRef::X509(der))?;
                at += len as usize;
            }
            let mut inner = CertVerifier::<Aes128GcmSha256, C, CERT_SIZE>::new(Certificate::X509(root));
            if let Some(host) = &self.hostname {
                TlsVerifier::<Aes128GcmSha256>::set_hostname_verification(&mut inner, host)?;
            }
            match TlsVerifier::<Aes128GcmSha256>::verify_certificate(&mut inner, &transcript, cert) {
                Ok(()) => return TlsVerifier::<Aes128GcmSha256>::verify_signature(&mut inner, CertificateVerifyRef { signature_scheme: scheme, signature: &sig }),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

impl<C: TlsClock> TlsVerifier<Aes128GcmSha256> for &mut DeferredVerifier<'_, C> {
    fn set_hostname_verification(&mut self, hostname: &str) -> Result<(), TlsError> {
        self.hostname = Some(heapless::String::try_from(hostname).map_err(|_| TlsError::InvalidCertificate)?);
        Ok(())
    }

    fn verify_certificate(&mut self, transcript: &Hash, cert: CertificateRef) -> Result<(), TlsError> {
        // Remember the chain exactly as it came; the real checks run in `finish`.
        self.chain.clear();
        self.lens.clear();
        for entry in cert.entries.iter() {
            let CertificateEntryRef::X509(der) = entry else { return Err(TlsError::DecodeError) };
            self.chain.extend_from_slice(der).map_err(|_| TlsError::OutOfMemory)?;
            self.lens.push(u16::try_from(der.len()).map_err(|_| TlsError::OutOfMemory)?).map_err(|_| TlsError::InsufficientSpace)?;
        }
        self.transcript = Some(transcript.clone());
        Ok(())
    }

    fn verify_signature(&mut self, verify: CertificateVerifyRef) -> Result<(), TlsError> {
        let mut sig = Vec::new();
        sig.extend_from_slice(verify.signature).map_err(|_| TlsError::OutOfMemory)?;
        self.signature = Some((verify.signature_scheme, sig));
        Ok(())
    }
}

/// Runs NTS-KE on `socket` (a connected TCP stream to port 4460 of `host`) and trusts a server whose chain ends at any of
/// `roots` (DER), and returns the session. `read_buf` must hold one TLS record from the server (16640 bytes is always enough, a few KiB is usually
/// enough); `write_buf` only needs to hold the ClientHello and the 16-byte request.
pub async fn key_exchange<S, C>(socket: S, host: &str, roots: &[&[u8]], read_buf: &mut [u8], write_buf: &mut [u8]) -> Result<Session, KeError>
where
    S: Read + Write,
    C: TlsClock,
{
    if C::now().is_none() {
        return Err(KeError::NoTime);
    }
    let alpn: [&[u8]; 1] = [nts::ALPN];
    let config = TlsConfig::new().with_server_name(host).with_alpn(&alpn);
    let mut tls = TlsConnection::<_, Aes128GcmSha256>::new(socket, read_buf, write_buf);
    let mut verifier = DeferredVerifier::<C>::new(roots);
    tls.open(TlsContext::new(&config, &mut verifier)).await.map_err(KeError::Tls)?;

    let request = nts::ke_request();
    let mut sent = 0;
    while sent < request.len() {
        sent += tls.write(&request[sent..]).await.map_err(KeError::Tls)?;
    }
    tls.flush().await.map_err(KeError::Tls)?;

    let mut message: Vec<u8, { nts::MAX_KE_RESPONSE }> = Vec::new();
    let len = loop {
        let mut chunk = [0u8; 256];
        let n = tls.read(&mut chunk).await.map_err(KeError::Tls)?;
        if n == 0 {
            return Err(KeError::Closed);
        }
        message.extend_from_slice(&chunk[..n]).map_err(|_| NtsError::BadKeMessage)?;
        if let Some(len) = nts::ke_message_len(&message)? {
            break len;
        }
    };
    let response = nts::parse_ke_response(&message[..len])?;

    let mut c2s = [0u8; nts::KEY_LEN];
    let mut s2c = [0u8; nts::KEY_LEN];
    tls.export_keying_material(nts::EXPORTER_LABEL, &nts::exporter_context(true), &mut c2s).map_err(KeError::Tls)?;
    tls.export_keying_material(nts::EXPORTER_LABEL, &nts::exporter_context(false), &mut s2c).map_err(KeError::Tls)?;
    let session = Session::new(c2s, s2c, response, host)?;
    let _ = tls.close().await; // best effort: everything we need has been received
    // Only now, with the connection done, the slow certificate and signature checks. Nothing above is trusted until
    // they pass, and `session` is not returned if they fail.
    verifier.finish().map_err(KeError::Tls)?;
    Ok(session)
}
