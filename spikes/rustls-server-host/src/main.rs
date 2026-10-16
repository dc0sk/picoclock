//! Serves one HTTPS response per connection on 127.0.0.1:PORT using rustls with the RustCrypto provider.
//! usage: rustls-server-host PORT cert.pem key-pkcs8.pem [connections]
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use pki_types::pem::PemObject;
use pki_types::{CertificateDer, PrivateKeyDer};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args[1].parse().unwrap();
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&args[2]).unwrap().map(Result::unwrap).collect();
    let key = PrivateKeyDer::from_pem_file(&args[3]).unwrap();
    let n: usize = args.get(4).map(|s| s.parse().unwrap()).unwrap_or(1);

    let provider = Arc::new(rustls_rustcrypto::provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3 with this provider")
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("a usable certificate and key");
    let config = Arc::new(config);

    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    eprintln!("listening on {port}");
    for _ in 0..n {
        let (mut sock, _) = listener.accept().unwrap();
        let mut conn = rustls::ServerConnection::new(config.clone()).unwrap();
        let t = std::time::Instant::now();
        let mut request = [0u8; 1024];
        let mut got = 0;
        // complete the handshake and read the request
        loop {
            if let Err(e) = conn.complete_io(&mut sock) {
                eprintln!("handshake failed: {e}");
                break;
            }
            if !conn.is_handshaking() {
                match conn.reader().read(&mut request[got..]) {
                    Ok(0) => break,
                    Ok(k) => {
                        got += k;
                        if request[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
            }
        }
        if got > 0 {
            let body = "hello over TLS from rustls + RustCrypto\n";
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = conn.writer().write_all(resp.as_bytes());
            let _ = conn.complete_io(&mut sock);
            conn.send_close_notify();
            let _ = conn.complete_io(&mut sock);
        }
        eprintln!(
            "connection done in {:?}: version {:?}, suite {:?}, group {:?}",
            t.elapsed(),
            conn.protocol_version(),
            conn.negotiated_cipher_suite().map(|s| s.suite()),
            conn.negotiated_key_exchange_group().map(|g| g.name()),
        );
    }
}
