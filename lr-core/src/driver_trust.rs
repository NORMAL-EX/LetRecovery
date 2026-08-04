//! Controlled WinPE trust initialization for signed boot-critical driver packages.
//!
//! Some older WinPE bases do not carry the complete Microsoft WHCP signing chain used by current
//! OEM drivers. DISM applies a stricter signature check to boot-critical drivers, so a missing
//! root *or intermediate CA* can reject an otherwise valid exported package. This module adds
//! only the pinned Microsoft chain to the volatile WinPE machine stores; it never modifies the
//! offline target Windows installation and never enables unsigned drivers.

use anyhow::{bail, Context, Result};
use base64::Engine;

const MICROSOFT_ROOT_CA_2010_SHA256: &str =
    "df545bf919a2439c36983b54cdfc903dfa4f37d3996d8d84b4c31eec6f3c163e";
const MICROSOFT_WINDOWS_THIRD_PARTY_COMPONENT_CA_2012_SHA256: &str =
    "9d08973e4d108da40a1a0b274180e17371134b4dd1621fa5c1f131b739b4b823";
const MICROSOFT_TIME_STAMP_PCA_2010_SHA256: &str =
    "ebec1edd9e140d9c105cc62b15a915c5443ddc514a35e5773c09afb0274c7ba5";

// Microsoft Root Certificate Authority 2010, SHA-1 thumbprint
// 3B1EFD3A66EA28B16697394703A72CA340A05BD5, valid 2010-06-23 through 2035-06-23.
const MICROSOFT_ROOT_CA_2010_DER_BASE64: &str = concat!(
    "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",
    "YbRA/1mGcdHVM2l8qXOKONdkDPFp"
);

// Microsoft Windows Third Party Component CA 2012, SHA-1 thumbprint
// 77A10EBF07542725218CD83A01B521C57BC67F73, valid 2012-04-18 through 2027-04-18.
// Current WHCP-signed packages, including boot-start VMware VMCI packages, can chain through
// this intermediate. A full Windows installation can cache/download it, while an offline WinPE
// may only contain the 2010 root.
const MICROSOFT_WINDOWS_THIRD_PARTY_COMPONENT_CA_2012_DER_BASE64: &str = concat!(
    "MIIF4TCCA8mgAwIBAgIKYQuqwQAAAAAACTANBgkqhkiG9w0BAQsFADCBiDELMAkGA1UEBhMC",
    "VVMxEzARBgNVBAgTCldhc2hpbmd0b24xEDAOBgNVBAcTB1JlZG1vbmQxHjAcBgNVBAoTFU1p",
    "Y3Jvc29mdCBDb3Jwb3JhdGlvbjEyMDAGA1UEAxMpTWljcm9zb2Z0IFJvb3QgQ2VydGlmaWNh",
    "dGUgQXV0aG9yaXR5IDIwMTAwHhcNMTIwNDE4MjM0ODM4WhcNMjcwNDE4MjM1ODM4WjCBjjEL",
    "MAkGA1UEBhMCVVMxEzARBgNVBAgTCldhc2hpbmd0b24xEDAOBgNVBAcTB1JlZG1vbmQxHjAc",
    "BgNVBAoTFU1pY3Jvc29mdCBDb3Jwb3JhdGlvbjE4MDYGA1UEAxMvTWljcm9zb2Z0IFdpbmRv",
    "d3MgVGhpcmQgUGFydHkgQ29tcG9uZW50IENBIDIwMTIwggEiMA0GCSqGSIb3DQEBAQUAA4IB",
    "DwAwggEKAoIBAQCjnDCECadjLs8KR/DqJPmjMCAPXlcxJoGaMQeyUNTOZwkIZQpapUuu1e0Q",
    "LuelmbWfaC+Yi1gCrCC0KcRxvSgcpf08m2TkxevfYSW88O5ov9Gny34qAoFOZFwMU4Z5Vxk3",
    "YbeY+QygTiJZm/kbLWc8JzxWkGbj/X9lfQ+GvTVH6IrM9NqO6WpOq6dV7KKJHtUzRVPL+Z53",
    "vc0s+QW4f3QBHej7GOFD0Q3pqtw3b73+uA/tHU0BRk4KrPyC6OxWgxOOOgHtFGR06mSyZhC2",
    "aG3IcAB9UEguPUPu4CSVxs2Ox/245JXP3X77lV6hAc1DsQfXpDDum4YaKm7BC1midG+LAgMB",
    "AAGjggFDMIIBPzAQBgkrBgEEAYI3FQEEAwIBADAdBgNVHQ4EFgQUYXGnh6//adUhdk9SkygA",
    "vnkSq4QwGQYJKwYBBAGCNxQCBAweCgBTAHUAYgBDAEEwCwYDVR0PBAQDAgGGMA8GA1UdEwEB",
    "/wQFMAMBAf8wHwYDVR0jBBgwFoAU1fZWy4/oolxiaNE9lJBb186aGMQwVgYDVR0fBE8wTTBL",
    "oEmgR4ZFaHR0cDovL2NybC5taWNyb3NvZnQuY29tL3BraS9jcmwvcHJvZHVjdHMvTWljUm9v",
    "Q2VyQXV0XzIwMTAtMDYtMjMuY3JsMFoGCCsGAQUFBwEBBE4wTDBKBggrBgEFBQcwAoY+aHR0",
    "cDovL3d3dy5taWNyb3NvZnQuY29tL3BraS9jZXJ0cy9NaWNSb29DZXJBdXRfMjAxMC0wNi0y",
    "My5jcnQwDQYJKoZIhvcNAQELBQADggIBAFqKZ9rM1f0NJkF3vwpGeLSz3hJpK3cjwmUvAV/S",
    "A/RhulCdLow5cvNsPmqxHnZt7LfzgtzMu8VpcChzZhc/VOvuARZIxEbZG4CugTqND3ltaLCe",
    "6i0/OdPKOH69XnwIbhncxsL0ODNoYeJSR4PhAAFW0rrLh4IFMQpBi07nf19f7V/TOS1F66IT",
    "v/0ewphBcWEWX8gKcCV8WWkxJORx5wq7BBf3n3IeydK7Gr49Av4JDLJDtFkamVOTliFf4Na3",
    "JgFClTasJ/2+9IV3aD0YvfS+mIgiEYZSFvNF7AOXEHCHo3BDcTzbyYYDFwz1c1vGfeFcZO3X",
    "xUjX7TLi0arTz6f2V05h+XfrZ/KIs94A2gOP0Io0Nz4d2GK40rHz4S+LcjuBlnxv/OxmdnJg",
    "GyTyoIltW20ALu8o3YaHBcK0ueW+ZMIq8koVXJjixCeF/1LjYn4PsgIL12bHCrLTPSAEFFAy",
    "WYMKfZvtWjgSAVK6L14gco5K8f3ncQKMO+EHvslz9N1H2LTvtKSzMLmJPnbKuQCYVn6r6oq4",
    "pdA4q2l3EwsUL+mqQR/3ur06KzSK7gqrY+Zj94gkjiANKzud48JJUqyfHw45O13UblBq5n1S",
    "Oqp8MxUpDSZeAVinTqk9eoRvdD9gn+QyTzYAr21x0z6mRmVfgXTx/sFx2kygQVqC3fEf",
);

// Microsoft Time-Stamp PCA 2010, SHA-1 thumbprint
// 36056A5662DCADECF82CC14C8B80EC5E0BCC59A6, valid 2021-09-30 through 2030-09-30.
// A current WHCP leaf can expire before an exported driver is restored. Authenticode then needs
// the countersignature chain to prove that the package was signed while the leaf was valid. Full
// Windows can obtain/cache this intermediate, but an offline WinPE commonly cannot.
const MICROSOFT_TIME_STAMP_PCA_2010_DER_BASE64: &str = concat!(
    "MIIHcTCCBVmgAwIBAgITMwAAABXF52ueAptJmQAAAAAAFTANBgkqhkiG9w0BAQsFADCBiDEL",
    "MAkGA1UEBhMCVVMxEzARBgNVBAgTCldhc2hpbmd0b24xEDAOBgNVBAcTB1JlZG1vbmQxHjAc",
    "BgNVBAoTFU1pY3Jvc29mdCBDb3Jwb3JhdGlvbjEyMDAGA1UEAxMpTWljcm9zb2Z0IFJvb3Qg",
    "Q2VydGlmaWNhdGUgQXV0aG9yaXR5IDIwMTAwHhcNMjEwOTMwMTgyMjI1WhcNMzAwOTMwMTgz",
    "MjI1WjB8MQswCQYDVQQGEwJVUzETMBEGA1UECBMKV2FzaGluZ3RvbjEQMA4GA1UEBxMHUmVk",
    "bW9uZDEeMBwGA1UEChMVTWljcm9zb2Z0IENvcnBvcmF0aW9uMSYwJAYDVQQDEx1NaWNyb3Nv",
    "ZnQgVGltZS1TdGFtcCBQQ0EgMjAxMDCCAiIwDQYJKoZIhvcNAQEBBQADggIPADCCAgoCggIB",
    "AOThpkzntHIhC3miy9ckeb0O1YLT/e6cBwfSqWxOdcjKNVf2AX9sSuDivbk+F2Az/1xPx2b3",
    "lVNxWuJ+Slr+uDZnhUYjDLWNE893MsAQGOhgfWpSg0S3po5GawcU88V29YZQ3MFEyHFcUTE3",
    "oAo4bo3t1w/YJlN8OWECesSq/XJprx2rrPY2vjUmZNqYO7oaezOtgFt+jBAcnVL+tuhiJdxq",
    "D89d9P6OU8/W7IVWTe/dvI2k45GPsjksUZzpcGkNyjYtcI4xyDUoveO0hyTD4MmPfrVUj9z6",
    "BVWYbWg7mka97aSueik3rMvrg0XnRm7KMtXAhjBcTyziYrLNueKNiOSWrAFKu75xqRdbZ2De",
    "+JKRHh09/SDPc31BmkZ1zcRfNN0Sidb9pSB9fvzZnkXftnIv231fgLrbqn427DZM9ituqBJR",
    "6L8FA6PRc6ZNN3SUHDSCD/AQ8rdHGO2n6Jl8P0zbr17C89XYcz1DTsEzOUyOArxCaC4Q6oRR",
    "RuLRvWoYWmEBc8pnol7XKHYC4jMYctenIPDC+hIK12NvDMk2ZItboKaDIV1fMHSRlJTYuVD5",
    "C4lh8zYGNRiER9vcG9H9stQcxWv2XFJRXRLbJbqvUAV6bMURHXLvjflSxIUXk8A8FdsaN8cI",
    "FRg/eKtFtvUeh17aj54WcmnGrnu3tz5q4i6tAgMBAAGjggHdMIIB2TASBgkrBgEEAYI3FQEE",
    "BQIDAQABMCMGCSsGAQQBgjcVAgQWBBQqp1L+ZMSavoKRPEY1Kc8Q/y8E7jAdBgNVHQ4EFgQU",
    "n6cVXQBeYl2D9OXSZacbUzUZ6XIwXAYDVR0gBFUwUzBRBgwrBgEEAYI3TIN9AQEwQTA/Bggr",
    "BgEFBQcCARYzaHR0cDovL3d3dy5taWNyb3NvZnQuY29tL3BraW9wcy9Eb2NzL1JlcG9zaXRv",
    "cnkuaHRtMBMGA1UdJQQMMAoGCCsGAQUFBwMIMBkGCSsGAQQBgjcUAgQMHgoAUwB1AGIAQwBB",
    "MAsGA1UdDwQEAwIBhjAPBgNVHRMBAf8EBTADAQH/MB8GA1UdIwQYMBaAFNX2VsuP6KJcYmjR",
    "PZSQW9fOmhjEMFYGA1UdHwRPME0wS6BJoEeGRWh0dHA6Ly9jcmwubWljcm9zb2Z0LmNvbS9w",
    "a2kvY3JsL3Byb2R1Y3RzL01pY1Jvb0NlckF1dF8yMDEwLTA2LTIzLmNybDBaBggrBgEFBQcB",
    "AQROMEwwSgYIKwYBBQUHMAKGPmh0dHA6Ly93d3cubWljcm9zb2Z0LmNvbS9wa2kvY2VydHMv",
    "TWljUm9vQ2VyQXV0XzIwMTAtMDYtMjMuY3J0MA0GCSqGSIb3DQEBCwUAA4ICAQCdVX38Kq3h",
    "LB9nATEkW+Geckv8qW/qXBS2Pk5HZHixBpOXPTEztTnXwnE2P9pkbHzQdTltuw8x5MKP+2zR",
    "oZQYIu7pZmc6U03dmLq2HnjYNi6cqYJWAAOwBb6J6Gngugnue99qb74py27YP0h1AdkY3m2C",
    "DPVtI1TkeFN1JFe53Z/zjj3G82jfZfakVqr3lbYoVSfQJL1AoL8ZthISEV09J+BAljis9/kp",
    "icO8F7BUhUKz/AyeixmJ5/ALaoHCgRlCGVJ1ijbCHcNhcy4sa3tuPywJeBTpkbKpW99Jo3QM",
    "vOyRgNI95ko+ZjtPu4b6MhrZlvSP9pEB9s7GdP32THJvEKt1MMU0sHrYUP4KWN1APMdUbZ1j",
    "dEgssU5HLcEUBHG/ZPkkvnNtyo4JvbMBV0lUZNlz138eW0QBjloZkWsNn6Qo3GcZKCS6OEua",
    "bvshVGtqRRFHqfG3rsjoiV5PndLQTHa1V1QJsWkBRH58oWFsc/4Ku+xBZj1p/cvBQUl+fpO+",
    "y/g75LcVv7TOPqUxUYS8vwLBgqJ7Fx0ViY1w/ue10CgaiQuPNtq6TPmb/wrpNPgkNWcr4A24",
    "5oyZ1uEi6vAnQj0llOZ0dFtq0Z4+7X6gMTN9vMvpe784cETRkPHIqzqKOghif9lwY1NNje6C",
    "baUFEMFxBmoQtB1VM1izoXBm8g==",
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

fn pinned_microsoft_windows_third_party_component_ca_2012() -> Result<Vec<u8>> {
    let der = base64::engine::general_purpose::STANDARD
        .decode(MICROSOFT_WINDOWS_THIRD_PARTY_COMPONENT_CA_2012_DER_BASE64)
        .context("decode embedded Microsoft Windows Third Party Component CA 2012")?;
    let actual = crate::hash::sha256_bytes(&der);
    if actual != MICROSOFT_WINDOWS_THIRD_PARTY_COMPONENT_CA_2012_SHA256 {
        bail!("embedded Microsoft Windows Third Party Component CA 2012 hash mismatch: {actual}");
    }
    Ok(der)
}

fn pinned_microsoft_time_stamp_pca_2010() -> Result<Vec<u8>> {
    let der = base64::engine::general_purpose::STANDARD
        .decode(MICROSOFT_TIME_STAMP_PCA_2010_DER_BASE64)
        .context("decode embedded Microsoft Time-Stamp PCA 2010")?;
    let actual = crate::hash::sha256_bytes(&der);
    if actual != MICROSOFT_TIME_STAMP_PCA_2010_SHA256 {
        bail!("embedded Microsoft Time-Stamp PCA 2010 hash mismatch: {actual}");
    }
    Ok(der)
}

/// Adds the pinned Microsoft root and intermediate needed by WHCP driver packages to WinPE's
/// volatile LocalMachine ROOT/CA stores. The operation is idempotent and must fail before DISM.
#[cfg(windows)]
pub fn ensure_pe_driver_signing_trust() -> Result<()> {
    use windows::Win32::Security::Cryptography::{
        CertAddEncodedCertificateToStore, CertCloseStore, CertFreeCertificateContext,
        CertOpenStore, CERT_CONTEXT, CERT_OPEN_STORE_FLAGS, CERT_STORE_ADD_USE_EXISTING,
        CERT_STORE_PROV_SYSTEM_W, CERT_SYSTEM_STORE_LOCAL_MACHINE, HCERTSTORE, HCRYPTPROV_LEGACY,
        X509_ASN_ENCODING,
    };

    struct CertificateStore(HCERTSTORE);
    impl Drop for CertificateStore {
        fn drop(&mut self) {
            unsafe {
                let _ = CertCloseStore(self.0, 0);
            }
        }
    }

    struct CertificateContext(*mut CERT_CONTEXT);
    impl Drop for CertificateContext {
        fn drop(&mut self) {
            unsafe {
                let _ = CertFreeCertificateContext(Some(self.0));
            }
        }
    }

    fn add_pinned_certificate(store_name: &str, der: &[u8], description: &str) -> Result<()> {
        let wide_store_name: Vec<u16> = store_name.encode_utf16().chain(Some(0)).collect();
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                X509_ASN_ENCODING,
                HCRYPTPROV_LEGACY::default(),
                CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_LOCAL_MACHINE),
                Some(wide_store_name.as_ptr().cast()),
            )
        }
        .with_context(|| format!("open WinPE LocalMachine {store_name} certificate store"))?;
        let store = CertificateStore(store);
        let mut certificate_context = std::ptr::null_mut();
        unsafe {
            CertAddEncodedCertificateToStore(
                store.0,
                X509_ASN_ENCODING,
                der,
                CERT_STORE_ADD_USE_EXISTING,
                Some(&mut certificate_context),
            )
        }
        .with_context(|| format!("add pinned {description} to WinPE {store_name} store"))?;
        if certificate_context.is_null() {
            bail!("WinPE {store_name} store returned no context for pinned {description}");
        }
        let certificate_context = CertificateContext(certificate_context);
        let stored = unsafe {
            std::slice::from_raw_parts(
                (*certificate_context.0).pbCertEncoded,
                (*certificate_context.0).cbCertEncoded as usize,
            )
        };
        if stored != der {
            bail!("WinPE {store_name} store read-back mismatch for pinned {description}");
        }
        Ok(())
    }

    let root = pinned_microsoft_root_ca_2010()?;
    add_pinned_certificate("ROOT", &root, "Microsoft Root Certificate Authority 2010")?;
    let intermediate = pinned_microsoft_windows_third_party_component_ca_2012()?;
    add_pinned_certificate(
        "CA",
        &intermediate,
        "Microsoft Windows Third Party Component CA 2012",
    )?;
    let timestamp_intermediate = pinned_microsoft_time_stamp_pca_2010()?;
    add_pinned_certificate(
        "CA",
        &timestamp_intermediate,
        "Microsoft Time-Stamp PCA 2010",
    )?;
    log::info!(
        "WinPE driver trust ready: root=3B1EFD3A66EA28B16697394703A72CA340A05BD5; \
         whcp_ca=77A10EBF07542725218CD83A01B521C57BC67F73; \
         timestamp_ca=36056A5662DCADECF82CC14C8B80EC5E0BCC59A6"
    );
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

    #[test]
    fn embedded_microsoft_whcp_intermediate_is_exactly_pinned() {
        let der = pinned_microsoft_windows_third_party_component_ca_2012().unwrap();
        assert!(der.starts_with(&[0x30, 0x82]));
        assert_eq!(
            crate::hash::sha256_bytes(&der),
            MICROSOFT_WINDOWS_THIRD_PARTY_COMPONENT_CA_2012_SHA256
        );
    }

    #[test]
    fn embedded_microsoft_timestamp_intermediate_is_exactly_pinned() {
        let der = pinned_microsoft_time_stamp_pca_2010().unwrap();
        assert!(der.starts_with(&[0x30, 0x82]));
        assert_eq!(
            crate::hash::sha256_bytes(&der),
            MICROSOFT_TIME_STAMP_PCA_2010_SHA256
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

        let certificates = [
            pinned_microsoft_root_ca_2010().unwrap(),
            pinned_microsoft_windows_third_party_component_ca_2012().unwrap(),
            pinned_microsoft_time_stamp_pca_2010().unwrap(),
        ];
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
            for der in certificates {
                CertAddEncodedCertificateToStore(
                    store,
                    X509_ASN_ENCODING,
                    &der,
                    CERT_STORE_ADD_USE_EXISTING,
                    None,
                )
                .unwrap();
            }
            CertCloseStore(store, 0).unwrap();
        }
    }
}
