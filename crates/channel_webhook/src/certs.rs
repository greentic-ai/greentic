use std::{net::SocketAddr, sync::Arc};
use axum::Router;
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer},
};
use rustls_pemfile::{certs, pkcs8_private_keys};
use tokio_rustls::TlsAcceptor;
use tokio::net::TcpListener;
use tokio_stream::StreamExt;
use hyper_util::{rt::{tokio::TokioIo, TokioExecutor}, service::TowerToHyperService};
use hyper_util::server::conn::auto::Builder as HyperBuilder;

type Certificate = CertificateDer<'static>;
type PrivateKey = PrivatePkcs8KeyDer<'static>;
pub async fn maybe_run_tls_server(app: Router, addr: SocketAddr, cert_opt: Option<String>, key_opt: Option<String>) -> anyhow::Result<bool> {


    if let (Some(cert_pem), Some(key_pem)) = (cert_opt, key_opt) {
        let certs = load_certs(&cert_pem)?;
        let key = load_key(&key_pem)?;

        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, rustls::pki_types::PrivateKeyDer::Pkcs8(key))?;
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind(addr).await?;
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let service = TowerToHyperService::new(app);

        tokio::spawn(async move {
            let mut incoming = incoming;
            while let Some(Ok(stream)) = incoming.next().await {
                let acceptor = acceptor.clone();
                let service = service.clone();
                tokio::spawn(async move {
                    match acceptor.accept(stream).await {
                        Ok(tls_stream) => {
                            let io = TokioIo::new(tls_stream);
                            if let Err(e) = HyperBuilder::new(TokioExecutor::new())
                                .serve_connection(io, service)
                                .await
                            {
                                eprintln!("TLS connection error: {:?}", e);
                            }
                        }
                        Err(e) => eprintln!("TLS handshake failed: {:?}", e),
                    }
                });
            }
        });

        Ok(true) // TLS server started
    } else {
        Ok(false) // Fallback to HTTP
    }
}

fn load_certs(pem: &str) -> anyhow::Result<Vec<Certificate>> {
    let mut reader = pem.as_bytes();
    let certs = certs(&mut reader)
        .filter_map(Result::ok)
        .collect();
    Ok(certs)
}

fn load_key(pem: &str) -> anyhow::Result<PrivateKey> {
    let mut reader = pem.as_bytes();
    let key = pkcs8_private_keys(&mut reader)
        .filter_map(Result::ok)
        .next()
        .ok_or_else(|| anyhow::anyhow!("No valid private key found"))?;

    Ok(key)
}
