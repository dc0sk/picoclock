//! TLS 1.3 server stream for the web page (Pico 2 W, `https` feature). `rustls` is used through its unbuffered API, which
//! needs no operating system: this file moves bytes between an `embassy-net` socket, `rustls`' state machine and the
//! caller, and implements `embedded_io_async` `Read`/`Write` over the decrypted data so the HTTP code does not know whether
//! it speaks to a TLS or a plain connection.
//!
//! One profile only (ADR-0002): TLS 1.3, AES-128-GCM with SHA-256, ECDSA P-256 certificate, X25519 or P-256 key exchange,
//! no session tickets (no resumption), no client certificates.

use alloc::sync::Arc;
use alloc::vec::Vec;

use embedded_io_async::{ErrorType, Read, Write};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, UnixTime};
use rustls::server::{ServerConfig, UnbufferedServerConnection};
use rustls::time_provider::TimeProvider;
use rustls::unbuffered::{AppDataRecord, ConnectionState, EncodeError, UnbufferedStatus};

use crate::state;

/// Buffer sizes. A TLS record is at most 16 KiB (16645 bytes on the wire); we cap what we send at 4 KiB (`max_fragment_size`).
/// The incoming buffer must hold one whole record of the client's, because a client may send full-size ones and a server
/// cannot ask for smaller: an update file (REQ-UPD-08) arrives that way, and with a smaller buffer the connection broke.
pub const INCOMING: usize = 17 * 1024;
pub const OUTGOING: usize = 6 * 1024;
const FRAGMENT: usize = 4096;

#[derive(Debug)]
pub enum TlsError {
    Io,
    Tls(rustls::Error),
    /// The peer closed (or the handshake never completed).
    Closed,
    /// The peer sent a record larger than our buffer.
    TooLarge,
}

impl core::fmt::Display for TlsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TlsError::Io => f.write_str("socket error"),
            TlsError::Tls(e) => write!(f, "TLS: {e}"),
            TlsError::Closed => f.write_str("connection closed"),
            TlsError::TooLarge => f.write_str("record or buffer too large"),
        }
    }
}

impl core::error::Error for TlsError {}

impl embedded_io_async::Error for TlsError {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        embedded_io_async::ErrorKind::Other
    }
}

/// `rustls` asks for the time only for session tickets, which are off; it still wants a provider.
#[derive(Debug)]
struct DeviceTime;
impl TimeProvider for DeviceTime {
    fn current_time(&self) -> Option<UnixTime> {
        state::status().utc_unix.map(|t| UnixTime::since_unix_epoch(core::time::Duration::from_secs(t.max(0) as u64)))
    }
}

/// The server configuration for one certificate chain (DER) and a PKCS#8 private key. Build once, share between connections.
pub fn server_config(chain: &[&[u8]], pkcs8: &[u8]) -> Result<Arc<ServerConfig>, rustls::Error> {
    let mut provider: CryptoProvider = rustls_rustcrypto::provider();
    // the cheapest suite on this CPU (the provider would otherwise prefer AES-256/SHA-384)
    provider.cipher_suites.retain(|s| s.suite() == rustls::CipherSuite::TLS13_AES_128_GCM_SHA256);
    let certs: Vec<CertificateDer<'static>> = chain.iter().map(|c| CertificateDer::from(c.to_vec())).collect();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8.to_vec()));
    let mut config = ServerConfig::builder_with_details(Arc::new(provider), Arc::new(DeviceTime))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.max_fragment_size = Some(FRAGMENT);
    config.send_tls13_tickets = 0;
    Ok(Arc::new(config))
}

pub struct TlsStream<'b, S: Read + Write> {
    sock: S,
    conn: UnbufferedServerConnection,
    incoming: &'b mut [u8],
    used: usize,
    outgoing: &'b mut [u8],
    /// Decrypted bytes not yet handed to the caller.
    pending: Vec<u8>,
    peer_closed: bool,
}

enum Goal<'a> {
    /// Until decrypted data is available (or the peer closed).
    Data,
    /// Until all of this has been encrypted and sent.
    Send(&'a [u8]),
    /// Until a close_notify has been sent.
    Close,
}

impl<'b, S: Read + Write> TlsStream<'b, S> {
    pub fn new(sock: S, config: Arc<ServerConfig>, incoming: &'b mut [u8], outgoing: &'b mut [u8]) -> Result<Self, TlsError> {
        let conn = UnbufferedServerConnection::new(config).map_err(TlsError::Tls)?;
        Ok(Self { sock, conn, incoming, used: 0, outgoing, pending: Vec::new(), peer_closed: false })
    }

    /// Runs the state machine until `goal` is reached.
    async fn drive(&mut self, mut goal: Goal<'_>) -> Result<(), TlsError> {
        let Self { sock, conn, incoming, used, outgoing, pending, peer_closed } = self;
        let mut out_used = 0usize;
        let mut sent = 0usize;
        loop {
            let UnbufferedStatus { mut discard, state } = conn.process_tls_records(&mut incoming[..*used]);
            let state = state.map_err(TlsError::Tls)?;
            match state {
                ConnectionState::ReadTraffic(mut st) => {
                    while let Some(rec) = st.next_record() {
                        let AppDataRecord { discard: d, payload } = rec.map_err(TlsError::Tls)?;
                        pending.extend_from_slice(payload);
                        discard += d;
                    }
                    if matches!(goal, Goal::Data) {
                        consume(incoming, used, discard);
                        return Ok(());
                    }
                }
                ConnectionState::EncodeTlsData(mut st) => match st.encode(&mut outgoing[out_used..]) {
                    Ok(n) => out_used += n,
                    Err(EncodeError::InsufficientSize(_)) => return Err(TlsError::TooLarge),
                    Err(EncodeError::AlreadyEncoded) => {}
                },
                ConnectionState::TransmitTlsData(st) => {
                    if out_used > 0 {
                        sock.write_all(&outgoing[..out_used]).await.map_err(|_| TlsError::Io)?;
                        sock.flush().await.map_err(|_| TlsError::Io)?;
                        out_used = 0;
                    }
                    st.done();
                }
                ConnectionState::BlockedHandshake => {
                    consume(incoming, used, discard);
                    discard = 0;
                    if *used == incoming.len() {
                        return Err(TlsError::TooLarge);
                    }
                    let n = sock.read(&mut incoming[*used..]).await.map_err(|_| TlsError::Io)?;
                    if n == 0 {
                        *peer_closed = true;
                        return Err(TlsError::Closed);
                    }
                    *used += n;
                }
                ConnectionState::WriteTraffic(mut st) => match &mut goal {
                    Goal::Send(data) => {
                        if sent == data.len() {
                            consume(incoming, used, discard);
                            return Ok(());
                        }
                        let chunk = &data[sent..data.len().min(sent + FRAGMENT)];
                        let n = st.encrypt(chunk, &mut outgoing[out_used..]).map_err(|_| TlsError::TooLarge)?;
                        out_used += n;
                        sent += chunk.len();
                        // send now so the output buffer is free for the next chunk
                        sock.write_all(&outgoing[..out_used]).await.map_err(|_| TlsError::Io)?;
                        out_used = 0;
                    }
                    Goal::Close => {
                        let n = st.queue_close_notify(&mut outgoing[out_used..]).map_err(|_| TlsError::TooLarge)?;
                        out_used += n;
                        sock.write_all(&outgoing[..out_used]).await.map_err(|_| TlsError::Io)?;
                        sock.flush().await.map_err(|_| TlsError::Io)?;
                        consume(incoming, used, discard);
                        return Ok(());
                    }
                    Goal::Data => {
                        // handshake finished; nothing buffered yet: wait for the client's request
                        consume(incoming, used, discard);
                        discard = 0;
                        if *used == incoming.len() {
                            return Err(TlsError::TooLarge);
                        }
                        let n = sock.read(&mut incoming[*used..]).await.map_err(|_| TlsError::Io)?;
                        if n == 0 {
                            *peer_closed = true;
                            return Ok(());
                        }
                        *used += n;
                    }
                },
                ConnectionState::PeerClosed | ConnectionState::Closed => {
                    *peer_closed = true;
                    consume(incoming, used, discard);
                    return if matches!(goal, Goal::Data) { Ok(()) } else { Err(TlsError::Closed) };
                }
                _ => return Err(TlsError::Closed),
            }
            consume(incoming, used, discard);
        }
    }

    /// Sends a close_notify and gives the socket back for closing.
    pub async fn close(mut self) -> S {
        let _ = self.drive(Goal::Close).await;
        self.sock
    }
}

/// Drops `discard` consumed bytes from the front of the incoming buffer.
fn consume(incoming: &mut [u8], used: &mut usize, discard: usize) {
    if discard > 0 {
        incoming.copy_within(discard..*used, 0);
        *used -= discard;
    }
}

impl<S: Read + Write> ErrorType for TlsStream<'_, S> {
    type Error = TlsError;
}

impl<S: Read + Write> Read for TlsStream<'_, S> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TlsError> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.pending.is_empty() && !self.peer_closed {
            self.drive(Goal::Data).await?;
        }
        let n = self.pending.len().min(buf.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

impl<S: Read + Write> Write for TlsStream<'_, S> {
    async fn write(&mut self, data: &[u8]) -> Result<usize, TlsError> {
        if data.is_empty() {
            return Ok(0);
        }
        self.drive(Goal::Send(data)).await?;
        Ok(data.len())
    }

    async fn flush(&mut self) -> Result<(), TlsError> {
        Ok(()) // every write is sent before it returns
    }
}
