//! Optional HTTPS for the remote control, with a certificate the app makes for itself.
//!
//! No authority has signed it, so browsers warn the first time. What it gives is that
//! the password and the session cookie no longer cross the network as plain text, and the
//! fingerprint shown in the settings lets the user check it is this computer they reached.
//!
//! The certificate is written out by hand (it is a few hundred bytes of DER) so that no
//! certificate-making library has to be added; signing uses the crypto library already in
//! the build.
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::digest;
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use tokio_rustls::TlsAcceptor;

use super::{hex, random, unhex};

const FILE: &str = "remote-certificate.json";
/// A little over a year, the most browsers accept.
const LIFE_DAYS: u64 = 397;
const DAY: u64 = 24 * 3600;

#[derive(Serialize, Deserialize)]
struct Saved {
    /// Both in hex.
    cert: String,
    key: String,
    /// Seconds since 1970 when the certificate stops being valid.
    until: u64,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The server side of TLS and the certificate's SHA-256 fingerprint. The certificate is
/// made once and kept in the data folder, and made again shortly before it runs out.
pub(crate) fn acceptor(data_dir: &Path) -> Result<(TlsAcceptor, String), String> {
    let path = data_dir.join(FILE);
    let kept = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
        .filter(|s| s.until > now() + DAY)
        .and_then(|s| Some((unhex(&s.cert)?, unhex(&s.key)?)));
    let (cert, key) = match kept {
        Some(pair) => pair,
        None => {
            let (cert, key, until) = make(now())?;
            let saved = Saved {
                cert: hex(&cert),
                key: hex(&key),
                until,
            };
            let text = serde_json::to_vec(&saved).map_err(|e| e.to_string())?;
            write_private(&path, &text).map_err(|e| format!("could not save the certificate: {e}"))?;
            (cert, key)
        }
    };
    let fingerprint = fingerprint(&cert);
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        )
        .map_err(|e| e.to_string())?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok((TlsAcceptor::from(Arc::new(config)), fingerprint))
}

/// Written so that only this user can read it: the key is in it.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// SHA-256 of the certificate, as browsers show it: `AB:CD:…`.
pub(crate) fn fingerprint(cert: &[u8]) -> String {
    digest::digest(&digest::SHA256, cert)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

// ---- DER, just as much as one certificate needs ----

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.extend([0x81, len as u8]);
    } else {
        out.extend([0x82, (len >> 8) as u8, len as u8]);
    }
    out.extend_from_slice(content);
    out
}

fn seq(parts: &[&[u8]]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

const ECDSA_SHA256: &[u8] = &[0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02];
const EC_PUBLIC_KEY: &[u8] = &[0x06, 0x07, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
const P256: &[u8] = &[0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
const COMMON_NAME: &[u8] = &[0x06, 0x03, 0x55, 0x04, 0x03];
const ALT_NAMES: &[u8] = &[0x06, 0x03, 0x55, 0x1D, 0x11];
const BASIC_CONSTRAINTS: &[u8] = &[0x06, 0x03, 0x55, 0x1D, 0x13];

/// `YYMMDDHHMMSSZ` for a time in seconds since 1970 (good until 2049).
fn utc_time(secs: u64) -> Vec<u8> {
    let days = (secs / DAY) as i64;
    let rest = secs % DAY;
    // Days to a date, from Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let text = format!(
        "{:02}{month:02}{day:02}{:02}{:02}{:02}Z",
        year % 100,
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    );
    tlv(0x17, text.as_bytes())
}

/// A new self-signed certificate, its PKCS#8 key and when it runs out.
fn make(now: u64) -> Result<(Vec<u8>, Vec<u8>, u64), String> {
    let rng = SystemRandom::new();
    let key = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
        .map_err(|_| "could not make a key".to_owned())?;
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, key.as_ref())
        .map_err(|_| "could not read the new key".to_owned())?;

    let mut serial = random(16);
    serial[0] = (serial[0] & 0x7F) | 0x01;
    let algorithm = seq(&[ECDSA_SHA256]);
    let name = seq(&[&tlv(0x31, &seq(&[COMMON_NAME, &tlv(0x0C, b"RustTorrent")]))]);
    let (from, until) = (now.saturating_sub(DAY), now + LIFE_DAYS * DAY);
    let mut point = vec![0u8];
    point.extend_from_slice(pair.public_key().as_ref());
    let public = seq(&[&seq(&[EC_PUBLIC_KEY, P256]), &tlv(0x03, &point)]);
    let names = seq(&[
        &tlv(0x82, b"localhost"),
        &tlv(0x87, &[127, 0, 0, 1]),
        &tlv(0x87, &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]),
    ]);
    let extensions = seq(&[
        &seq(&[ALT_NAMES, &tlv(0x04, &names)]),
        // Critical, and not an authority: it cannot be used to sign other certificates.
        &seq(&[BASIC_CONSTRAINTS, &[0x01, 0x01, 0xFF], &tlv(0x04, &seq(&[]))]),
    ]);
    let tbs = seq(&[
        &[0xA0, 0x03, 0x02, 0x01, 0x02],
        &tlv(0x02, &serial),
        &algorithm,
        &name,
        &seq(&[&utc_time(from), &utc_time(until)]),
        &name,
        &public,
        &tlv(0xA3, &extensions),
    ]);
    let signature = pair
        .sign(&rng, &tbs)
        .map_err(|_| "could not sign the certificate".to_owned())?;
    let mut bits = vec![0u8];
    bits.extend_from_slice(signature.as_ref());
    let cert = seq(&[&tbs, &algorithm, &tlv(0x03, &bits)]);
    Ok((cert, key.as_ref().to_vec(), until))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(utc_time(0)[2..], *b"700101000000Z");
        // 2026-11-15 12:34:56
        assert_eq!(utc_time(1_794_746_096)[2..], *b"261115123456Z");
        assert_eq!(utc_time(951_782_400)[2..], *b"000229000000Z");
    }

    #[test]
    fn lengths() {
        assert_eq!(tlv(4, &[0; 5])[..2], [4, 5]);
        assert_eq!(tlv(4, &[0; 200])[..3], [4, 0x81, 200]);
        assert_eq!(tlv(4, &[0; 300])[..4], [4, 0x82, 1, 44]);
    }

    #[test]
    fn made_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("rusttorrent-tls-{}", hex(&random(6))));
        std::fs::create_dir_all(&dir).unwrap();
        let (_, first) = acceptor(&dir).expect("rustls takes the certificate");
        let (_, again) = acceptor(&dir).unwrap();
        assert_eq!(first, again);
        assert_eq!(first.len(), 32 * 3 - 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
