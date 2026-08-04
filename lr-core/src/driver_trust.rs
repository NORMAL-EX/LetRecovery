//! Controlled WinPE trust initialization for signed boot-critical driver packages.
//!
//! Some older WinPE bases do not carry Microsoft Root Certificate Authority 2010 even though
//! currently installed WHCP driver catalogs still chain to it. DISM applies a stricter signature
//! check to boot-critical drivers, so a missing root can reject an otherwise valid exported
//! package. This module adds only the pinned Microsoft root to the volatile WinPE machine store;
//! it never modifies the offline target Windows installation and never enables unsigned drivers.

use anyhow::{bail, Context, Result};
use base64::Engine;

const MICROSOFT_ROOT_CA_2010_SHA256: &str =
    "df545bf919a2439c36983b54cdfc903dfa4f37d3996d8d84b4c31eec6f3c163e";

// Microsoft Root Certificate Authority 2010, SHA-1 thumbprint
// 3B1EFD3A66EA28B16697394703A72CA340A05BD5, valid 2010-06-23 through 2035-06-23.
const MICROSOFT_ROOT_CA_2010_DER_BASE64: &str = concat!(
    "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",
    "YbRA/1mGcdHVM2l8qXOKONdkDPFp"
);

fn pinned_microsoft_root_ca_2010() -> Result<Vec<u8>> {
    let der = base64::engine::general_purpose::STANDARD
        .decode(MICROSOFT_ROOT_CA_2010_DER_BASE64)
        .context("decode embedded Microsoft Root Certificate Authority 2010")?;
    let actual = crate::hash::sha256_bytes(&der);
    if actual != MICROSOFT_ROOT_CA_2010_SHA256 {
        bail!("embedded Microsoft Root Certificate Authority 2010 hash mismatch: {actual}");
    }
    Ok(der)
}

/// Adds the pinned Microsoft root needed by older WHCP driver catalogs to WinPE's volatile
/// LocalMachine ROOT store. The operation is idempotent and must fail before DISM is started.
#[cfg(windows)]
pub fn ensure_pe_driver_signing_trust() -> Result<()> {
    use windows::Win32::Security::Cryptography::{
        CertAddEncodedCertificateToStore, CertCloseStore, CertOpenStore, CERT_OPEN_STORE_FLAGS,
        CERT_STORE_ADD_USE_EXISTING, CERT_STORE_PROV_SYSTEM_W, CERT_SYSTEM_STORE_LOCAL_MACHINE,
        HCERTSTORE, HCRYPTPROV_LEGACY, X509_ASN_ENCODING,
    };

    struct CertificateStore(HCERTSTORE);
    impl Drop for CertificateStore {
        fn drop(&mut self) {
            unsafe {
                let _ = CertCloseStore(self.0, 0);
            }
        }
    }

    let der = pinned_microsoft_root_ca_2010()?;
    let store_name: Vec<u16> = "ROOT\0".encode_utf16().collect();
    let store = unsafe {
        CertOpenStore(
            CERT_STORE_PROV_SYSTEM_W,
            X509_ASN_ENCODING,
            HCRYPTPROV_LEGACY::default(),
            CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_LOCAL_MACHINE),
            Some(store_name.as_ptr().cast()),
        )
    }
    .context("open WinPE LocalMachine ROOT certificate store")?;
    let store = CertificateStore(store);
    unsafe {
        CertAddEncodedCertificateToStore(
            store.0,
            X509_ASN_ENCODING,
            &der,
            CERT_STORE_ADD_USE_EXISTING,
            None,
        )
    }
    .context("add pinned Microsoft Root Certificate Authority 2010 to WinPE trust store")?;
    Ok(())
}

#[cfg(not(windows))]
pub fn ensure_pe_driver_signing_trust() -> Result<()> {
    bail!("WinPE driver trust initialization is only available on Windows")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_microsoft_root_is_exactly_pinned() {
        let der = pinned_microsoft_root_ca_2010().unwrap();
        assert!(der.starts_with(&[0x30, 0x82]));
        assert_eq!(
            crate::hash::sha256_bytes(&der),
            MICROSOFT_ROOT_CA_2010_SHA256
        );
    }

    #[cfg(windows)]
    #[test]
    fn cryptoapi_accepts_the_pinned_der_without_touching_system_stores() {
        use windows::Win32::Security::Cryptography::{
            CertAddEncodedCertificateToStore, CertCloseStore, CertOpenStore, CERT_OPEN_STORE_FLAGS,
            CERT_STORE_ADD_USE_EXISTING, CERT_STORE_PROV_MEMORY, HCRYPTPROV_LEGACY,
            X509_ASN_ENCODING,
        };

        let der = pinned_microsoft_root_ca_2010().unwrap();
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_MEMORY,
                X509_ASN_ENCODING,
                HCRYPTPROV_LEGACY::default(),
                CERT_OPEN_STORE_FLAGS::default(),
                None,
            )
        }
        .unwrap();
        unsafe {
            CertAddEncodedCertificateToStore(
                store,
                X509_ASN_ENCODING,
                &der,
                CERT_STORE_ADD_USE_EXISTING,
                None,
            )
            .unwrap();
            CertCloseStore(store, 0).unwrap();
        }
    }
}
