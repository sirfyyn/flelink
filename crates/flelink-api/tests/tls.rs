//! Host mode over https: the certificate is made once and kept, its fingerprint is what the client pins, and requests over TLS
//! reach the same routes (the access key still applies to other machines).
use flelink_api::server::{new_studio_lan, Options};
use flelink_api::tls::{fingerprint, load_or_create, serve_tls};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio_rustls::TlsConnector;

struct Tmp(std::path::PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Accepts any certificate and remembers its fingerprint: the test pins by comparing it afterwards.
#[derive(Debug)]
struct Pin(Mutex<String>);
impl ServerCertVerifier for Pin {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
        *self.0.lock().unwrap() = fingerprint(end_entity);
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PSS_SHA256,
        ]
    }
}

#[test]
fn the_certificate_is_kept_and_https_reaches_the_routes() {
    let dir = Tmp(std::env::temp_dir().join(format!("flelink-tls-{}", std::process::id())));
    let first = load_or_create(&dir.0.join("tls")).expect("make");
    let again = load_or_create(&dir.0.join("tls")).expect("reload");
    assert_eq!(
        first.fingerprint, again.fingerprint,
        "the certificate is made once"
    );
    assert_eq!(first.fingerprint.split(':').count(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.0.join("tls/key.pem"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the key is private to its user");
    }

    let st = new_studio_lan(
        Options {
            home: dir.0.join("home"),
            web_dir: None,
            version: "test".into(),
            github_api: flelink_api::engines::GITHUB_API.into(),
        },
        true,
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_tls(st, l, first.config.clone(), async {
            let _ = stop_rx.await;
        }));

        let pin = Arc::new(Pin(Mutex::new(String::new())));
        let mut cfg = ClientConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(pin.clone())
        .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let mut tls = TlsConnector::from(Arc::new(cfg))
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .expect("handshake");
        tls.write_all(
            format!(
                "GET /api/ping HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut out = vec![];
        let _ = tls.read_to_end(&mut out).await;
        let text = String::from_utf8_lossy(&out);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("\"app\":\"fle-studio\""), "{text}");
        assert_eq!(
            *pin.0.lock().unwrap(),
            first.fingerprint,
            "the client saw the certificate the host prints"
        );
        let _ = stop_tx.send(());
        let _ = server.await;
    });
}
