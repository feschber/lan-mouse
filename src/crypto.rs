use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::{fs::File, io::BufReader};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use sha2::{Digest, Sha256};
use thiserror::Error;
use webrtc_dtls::crypto::Certificate;

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
    #[error("a file containing the public key is missing")]
    PkMissing,
    #[error("a file containing the private key is missing")]
    SkMissing,
}

pub fn generate_fingerprint(cert: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(cert);
    let bytes = hash
        .finalize()
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>();
    let fingerprint = bytes.join(":").to_lowercase();
    fingerprint
}

pub fn certificate_fingerprint(cert: &Certificate) -> String {
    let certificate = cert.certificate.get(0).expect("certificate missing");
    generate_fingerprint(certificate)
}

/// load certificate from file
pub fn load_certificate(path: &Path) -> Result<Certificate, Error> {
    let f = File::open(path)?;

    let mut reader = BufReader::new(f);
    let mut pem = String::new();
    reader.read_to_string(&mut pem)?;
    Ok(Certificate::from_pem(pem.as_str())?)
}

pub(crate) fn load_or_generate_key_and_cert(path: &Path) -> Result<Certificate, Error> {
    if path.exists() && path.is_file() {
        return Ok(load_certificate(path)?);
    } else {
        return Ok(generate_key_and_cert(path)?);
    }
}

pub(crate) fn generate_key_and_cert(path: &Path) -> Result<Certificate, Error> {
    let cert = Certificate::generate_self_signed(["ignored".to_owned()])?;
    let serialized = cert.serialize_pem();
    let f = File::create(path)?;
    #[cfg(unix)]
    {
        let mut perm = f.metadata()?.permissions();
        perm.set_mode(0o400); /* r-- --- --- */
        f.set_permissions(perm)?;
    }
    /* FIXME windows permissions */
    let mut writer = BufWriter::new(f);
    writer.write(serialized.as_bytes())?;
    Ok(cert)
}
