use std::io::{self, Read};
use std::path::PathBuf;
use std::{fs::File, io::BufReader};

use rcgen::KeyPair;
use rustls::pki_types::CertificateDer;
use thiserror::Error;
use webrtc_dtls::crypto::{Certificate, CryptoPrivateKey};

#[derive(Debug, Error)]
pub enum Error {
    #[error("block is not a private key, unable to load key")]
    ErrBlockIsNotPrivateKey,
    #[error("unknown key time in PKCS#8 wrapping, unable to load key")]
    ErrUnknownKeyTime,
    #[error("no private key found, unable to load key")]
    ErrNoPrivateKeyFound,
    #[error("block is not a certificate, unable to load certificates")]
    ErrBlockIsNotCertificate,
    #[error("no certificate found, unable to load certificates")]
    ErrNoCertificateFound,

    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Dtls(#[from] webrtc_dtls::Error),
    #[error("{0}")]
    Other(String),
}

/// load_key_and_certificate reads certificates or key from file
pub fn load_key_and_certificate(
    key_path: PathBuf,
    certificate_path: PathBuf,
) -> Result<Certificate, Error> {
    let private_key = load_key(key_path)?;

    let certificate = load_certificate(certificate_path)?;

    Ok(Certificate {
        certificate,
        private_key,
    })
}

/// load_key Load/read key from file
pub fn load_key(path: PathBuf) -> Result<CryptoPrivateKey, Error> {
    let f = File::open(path)?;
    let mut reader = BufReader::new(f);
    let mut buf = vec![];
    reader.read_to_end(&mut buf)?;

    let s = String::from_utf8(buf).expect("utf8 of file");

    let key_pair = KeyPair::from_pem(s.as_str()).expect("key pair in file");

    Ok(CryptoPrivateKey::from_key_pair(&key_pair).expect("crypto key pair"))
}

/// load_certificate Load/read certificate(s) from file
pub fn load_certificate(path: PathBuf) -> Result<Vec<CertificateDer<'static>>, Error> {
    let f = File::open(path)?;

    let mut reader = BufReader::new(f);
    match rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>() {
        Ok(certs) => Ok(certs.into_iter().map(CertificateDer::from).collect()),
        Err(_) => Err(Error::ErrNoCertificateFound),
    }
}
