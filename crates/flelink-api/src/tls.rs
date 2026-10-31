//! TLS for host mode: a certificate of our own making (ECDSA P-256, kept in `<data>/tls/`), its SHA-256 fingerprint for the
//! client to pin, and a server loop that speaks https. Like the Python Studio's Remote access plugin, without its pairing.
use crate::server::{router, Ctx};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;
use tower::Service;

pub struct Identity {
    pub config: Arc<ServerConfig>,
    /// SHA-256 of the certificate, as `AA:BB:...`
    pub fingerprint: String,
}

/// The fingerprint of a DER certificate, the way browsers and `openssl x509 -fingerprint` show it.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn names() -> Vec<String> {
    let mut n = vec!["localhost".to_string(), "127.0.0.1".into(), "::1".into()];
    let own = crate::server::machine_name();
    if !own.is_empty() {
        n.push(own);
    }
    n
}

/// Load `<dir>/cert.pem` and `<dir>/key.pem`, making them first if they are not there.
pub fn load_or_create(dir: &Path) -> Result<Identity, String> {
    let (cert_p, key_p) = (dir.join("cert.pem"), dir.join("key.pem"));
    if !(cert_p.exists() && key_p.exists()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let ck = rcgen::generate_simple_self_signed(names()).map_err(|e| e.to_string())?;
        std::fs::write(&cert_p, ck.cert.pem()).map_err(|e| e.to_string())?;
        std::fs::write(&key_p, ck.key_pair.serialize_pem()).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&key_p, std::fs::Permissions::from_mode(0o600));
        }
    }
    let cert_pem = std::fs::read(&cert_p).map_err(|e| e.to_string())?;
    let key_pem = std::fs::read(&key_p).map_err(|e| e.to_string())?;
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile_certs(&cert_pem)?;
    let key = rustls_pemfile_key(&key_pem)?;
    let fp = fingerprint(certs.first().ok_or("no certificate in cert.pem")?);
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| e.to_string())?;
    Ok(Identity {
        config: Arc::new(config),
        fingerprint: fp,
    })
}

/// PEM blocks are base64 between BEGIN/END lines; a few lines of code keep a whole PEM crate out.
fn pem_blocks(pem: &[u8], label: &str) -> Vec<Vec<u8>> {
    use base64::Engine as _;
    let text = String::from_utf8_lossy(pem);
    let (begin, end) = (
        format!("-----BEGIN {label}-----"),
        format!("-----END {label}-----"),
    );
    let mut out = vec![];
    let mut rest = &text[..];
    while let Some(i) = rest.find(&begin) {
        let after = &rest[i + begin.len()..];
        let Some(j) = after.find(&end) else { break };
        let b64: String = after[..j].chars().filter(|c| !c.is_whitespace()).collect();
        if let Ok(d) = base64::engine::general_purpose::STANDARD.decode(b64) {
            out.push(d);
        }
        rest = &after[j + end.len()..];
    }
    out
}

fn rustls_pemfile_certs(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, String> {
    let v: Vec<_> = pem_blocks(pem, "CERTIFICATE")
        .into_iter()
        .map(CertificateDer::from)
        .collect();
    if v.is_empty() {
        Err("cert.pem has no certificate".into())
    } else {
        Ok(v)
    }
}

fn rustls_pemfile_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, String> {
    if let Some(k) = pem_blocks(pem, "PRIVATE KEY").into_iter().next() {
        return Ok(PrivateKeyDer::Pkcs8(k.into()));
    }
    if let Some(k) = pem_blocks(pem, "EC PRIVATE KEY").into_iter().next() {
        return Ok(PrivateKeyDer::Sec1(k.into()));
    }
    Err("key.pem has no private key".into())
}

/// Serve https on `listener` until `shutdown` completes.
pub async fn serve_tls(
    st: Ctx,
    listener: tokio::net::TcpListener,
    config: Arc<ServerConfig>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let app = router(st).into_make_service_with_connect_info::<SocketAddr>();
    let acceptor = TlsAcceptor::from(config);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let (tcp, peer) = match accepted { Ok(x) => x, Err(_) => continue };
                let mut app = app.clone();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    let Ok(svc) = app.call(peer).await;
                    let _ = Builder::new(TokioExecutor::new())
                        .serve_connection_with_upgrades(TokioIo::new(tls), TowerToHyperService::new(svc))
                        .await;
                });
            }
        }
    }
}
