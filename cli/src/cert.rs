//! The certificate the shell is signed with (shell/sign.sh), and its entry
//! in the machine's Trusted Root store.
//!
//! The shell asks for uiAccess, which Windows grants only to an exe whose
//! signature chains to a root in that store:
//! https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-10/security/threat-protection/security-policy-settings/user-account-control-only-elevate-uiaccess-applications-that-are-installed-in-secure-locations
//! Every build of the shell gets a certificate of its own whose key is
//! thrown away after signing, so the certificate is read back out of the
//! exe's signature rather than kept anywhere: the exe and the certificate
//! trusted for it can't disagree.

use anyhow::{Context, Result, bail, ensure};
use sha1::{Digest, Sha1};

/// Root store entries are keys under this, in the SOFTWARE hive, named by
/// the certificate's SHA-1 thumbprint.
pub const ROOT_STORE: &str = "Microsoft\\SystemCertificates\\ROOT\\Certificates";
/// Our certificates' subject and issuer, `CN=defenestrate shell`, which
/// sign.sh sets. It's how `update` tells a previous build's certificate
/// from everyone else's.
const COMMON_NAME: &str = "defenestrate shell";

/// A self-signed certificate with our subject: the only kind this ever
/// puts into Root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCert {
    der: Vec<u8>,
}

impl ShellCert {
    /// The certificate in a signed exe's Authenticode signature, if it's
    /// one of ours. None for an unsigned exe, or one signed by anyone else:
    /// some other publisher's certificate has no business in Root.
    pub fn from_exe(exe: &[u8]) -> Result<Option<Self>> {
        let Some(pkcs7) = authenticode(exe)? else { return Ok(None) };
        let certs = signed_data_certificates(pkcs7).context("reading the exe's Authenticode signature")?;
        Ok(certs.into_iter().find_map(|c| Self::from_der(c).ok()))
    }

    /// `der` if it's a certificate of ours.
    pub fn from_der(der: &[u8]) -> Result<Self> {
        let (subject, issuer) = names(der)?;
        let ours = our_name();
        ensure!(subject == ours && issuer == ours, "not a defenestrate shell certificate");
        Ok(Self { der: der.to_vec() })
    }

    /// The upper case hex SHA-1 Windows names the store's key by.
    pub fn thumbprint(&self) -> String {
        Sha1::digest(&self.der).iter().map(|b| format!("{b:02X}")).collect()
    }

    /// The key under SOFTWARE holding this certificate in Root.
    pub fn root_key(&self) -> String {
        format!("{ROOT_STORE}\\{}", self.thumbprint())
    }

    /// The `Blob` value of its Root store key: CryptoAPI's serialized
    /// certificate, properties then the certificate itself, as [MS-GPEF]
    /// specifies for the same store entries deployed by Group Policy:
    /// https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-gpef/6a9e35fa-2ac7-4c10-81e1-eabe8d2472f1
    /// The friendly name is only for certmgr.msc.
    pub fn blob(&self) -> Vec<u8> {
        let name: Vec<u8> = COMMON_NAME.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        let mut out = Vec::new();
        property(&mut out, FRIENDLY_NAME_PROP_ID, &name);
        property(&mut out, CERT_PROP_ID, &self.der);
        out
    }
}

const FRIENDLY_NAME_PROP_ID: u32 = 11;
const CERT_PROP_ID: u32 = 0x20;

/// The certificate in a Root store `Blob`, if there is one.
pub fn cert_in_blob(mut blob: &[u8]) -> Option<&[u8]> {
    while blob.len() >= 12 {
        let word = |i: usize| u32::from_le_bytes(blob[i..i + 4].try_into().unwrap());
        let (id, len) = (word(0), word(8) as usize);
        let value = blob.get(12..12 + len)?;
        if id == CERT_PROP_ID {
            return Some(value);
        }
        blob = &blob[12 + len..];
    }
    None
}

fn property(out: &mut Vec<u8>, id: u32, value: &[u8]) {
    out.extend(id.to_le_bytes());
    // "Reserved. MUST be set to 0x01 0x00 0x00 0x00."
    out.extend(1u32.to_le_bytes());
    out.extend((value.len() as u32).to_le_bytes());
    out.extend(value);
}

/// DER of the Name `CN=defenestrate shell`, as OpenSSL writes it (a
/// UTF8String, its default string_mask).
fn our_name() -> Vec<u8> {
    let cn = tlv(0x0c, COMMON_NAME.as_bytes());
    let attr = tlv(0x30, &[tlv(0x06, &[0x55, 0x04, 0x03]), cn].concat());
    tlv(0x30, &tlv(0x31, &attr))
}

fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    match value.len() {
        n @ 0..0x80 => out.push(n as u8),
        n @ 0x80..0x100 => out.extend([0x81, n as u8]),
        n => out.extend([0x82, (n >> 8) as u8, n as u8]),
    }
    out.extend(value);
    out
}

/// The PKCS#7 SignedData in a PE's certificate table, if it has one.
/// Layout: https://learn.microsoft.com/en-us/windows/win32/debug/pe-format
fn authenticode(pe: &[u8]) -> Result<Option<&[u8]>> {
    let u16_at = |i: usize| pe.get(i..i + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap()));
    let u32_at = |i: usize| pe.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let short = || anyhow::anyhow!("not a PE file");
    ensure!(pe.starts_with(b"MZ"), "not a PE file");
    let pe_header = u32_at(0x3c).ok_or_else(short)? as usize;
    ensure!(pe.get(pe_header..pe_header + 4) == Some(b"PE\0\0"), "not a PE file");
    let optional = pe_header + 24;
    // The data directories follow the optional header's fixed fields,
    // whose size depends on PE32 vs PE32+.
    let (count_at, directories) = match u16_at(optional).ok_or_else(short)? {
        0x10b => (optional + 92, optional + 96),
        0x20b => (optional + 108, optional + 112),
        magic => bail!("unknown PE optional header magic {magic:#x}"),
    };
    let count = u32_at(count_at).ok_or_else(short)?;
    const SECURITY: u32 = 4;
    if count <= SECURITY {
        return Ok(None);
    }
    // The certificate table's entry is a file offset, not an RVA.
    let entry = directories + 8 * SECURITY as usize;
    let (offset, size) = (u32_at(entry).ok_or_else(short)? as usize, u32_at(entry + 4).ok_or_else(short)? as usize);
    if size == 0 {
        return Ok(None);
    }
    let table = pe.get(offset..offset + size).context("the certificate table runs past the end of the file")?;
    // WIN_CERTIFICATE: dwLength, wRevision, wCertificateType, bCertificate.
    let len = u32::from_le_bytes(table.get(0..4).context("truncated WIN_CERTIFICATE")?.try_into()?) as usize;
    let kind = u16::from_le_bytes(table[6..8].try_into()?);
    const WIN_CERT_TYPE_PKCS_SIGNED_DATA: u16 = 2;
    ensure!(kind == WIN_CERT_TYPE_PKCS_SIGNED_DATA, "the exe's certificate table holds type {kind}, not a PKCS#7 signature");
    Ok(Some(table.get(8..len).context("truncated WIN_CERTIFICATE")?))
}

/// One DER element: its tag, its contents, and what comes after it.
fn element(der: &[u8]) -> Result<(u8, &[u8], &[u8])> {
    let (&tag, rest) = der.split_first().context("truncated DER")?;
    let (&first, rest) = rest.split_first().context("truncated DER")?;
    let (len, rest) = match first {
        n if n < 0x80 => (n as usize, rest),
        n @ 0x81..=0x84 => {
            let k = (n & 0x7f) as usize;
            let bytes = rest.get(..k).context("truncated DER")?;
            (bytes.iter().fold(0usize, |a, &b| a << 8 | b as usize), &rest[k..])
        }
        n => bail!("unsupported DER length byte {n:#x}"),
    };
    let contents = rest.get(..len).context("truncated DER")?;
    Ok((tag, contents, &rest[len..]))
}

/// The whole element (tag and length included) at the start of `der`.
fn whole(der: &[u8]) -> Result<&[u8]> {
    let (_, _, rest) = element(der)?;
    Ok(&der[..der.len() - rest.len()])
}

fn expect(der: &[u8], want: u8) -> Result<(&[u8], &[u8])> {
    let (tag, contents, rest) = element(der)?;
    ensure!(tag == want, "expected DER tag {want:#04x}, found {tag:#04x}");
    Ok((contents, rest))
}

/// The certificates field of a ContentInfo holding a SignedData (RFC 5652).
fn signed_data_certificates(content_info: &[u8]) -> Result<Vec<&[u8]>> {
    let (ci, _) = expect(content_info, 0x30)?;
    let (oid, rest) = expect(ci, 0x06)?;
    ensure!(oid == [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02], "not a PKCS#7 SignedData");
    let (explicit, _) = expect(rest, 0xa0)?;
    let (sd, _) = expect(explicit, 0x30)?;
    let (_version, rest) = expect(sd, 0x02)?;
    let (_digests, rest) = expect(rest, 0x31)?;
    let (_encap, rest) = expect(rest, 0x30)?;
    let (tag, mut certs, _) = element(rest)?;
    if tag != 0xa0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    while !certs.is_empty() {
        let (tag, _, rest) = element(certs)?;
        // Only X.509 certificates (SEQUENCE); skip the other choices.
        if tag == 0x30 {
            out.push(whole(certs)?);
        }
        certs = rest;
    }
    Ok(out)
}

/// A certificate's subject and issuer Names, as DER.
fn names(cert: &[u8]) -> Result<(&[u8], &[u8])> {
    let (c, _) = expect(cert, 0x30)?;
    let (tbs, _) = expect(c, 0x30)?;
    let mut rest = tbs;
    if element(rest)?.0 == 0xa0 {
        rest = element(rest)?.2;
    }
    let (_serial, rest) = expect(rest, 0x02)?;
    let (_alg, rest) = expect(rest, 0x30)?;
    let issuer = whole(rest)?;
    let (_, rest) = expect(rest, 0x30)?;
    let (_validity, rest) = expect(rest, 0x30)?;
    let subject = whole(rest)?;
    expect(rest, 0x30)?;
    Ok((subject, issuer))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A copy of the shell's PE headers and certificate table as sign.sh
    // left them, with the code in between dropped and the table's offset
    // patched to match: see tests/fixtures/make-signed-headers.py.
    const SIGNED: &[u8] = include_bytes!("../tests/fixtures/signed-headers.exe");
    const THUMBPRINT: &str = include_str!("../tests/fixtures/signed-headers.thumbprint");

    #[test]
    fn reads_our_certificate_out_of_a_signed_exe() {
        let cert = ShellCert::from_exe(SIGNED).unwrap().expect("no certificate found");
        assert_eq!(cert.thumbprint(), THUMBPRINT.trim());
        assert_eq!(cert.root_key(), format!("Microsoft\\SystemCertificates\\ROOT\\Certificates\\{}", THUMBPRINT.trim()));
    }

    #[test]
    fn unsigned_exe_has_none() {
        let mut unsigned = SIGNED.to_vec();
        let pe = u32::from_le_bytes(unsigned[0x3c..0x40].try_into().unwrap()) as usize;
        let entry = pe + 24 + 112 + 8 * 4;
        unsigned[entry..entry + 8].fill(0);
        assert_eq!(ShellCert::from_exe(&unsigned).unwrap(), None);
        assert!(ShellCert::from_exe(b"not an exe").is_err());
    }

    #[test]
    fn only_our_subject() {
        let cert = ShellCert::from_exe(SIGNED).unwrap().unwrap();
        let mut other = cert.der.clone();
        // "defenestrate shell" -> "Defenestrate shell", in both names.
        for i in 0..other.len() - COMMON_NAME.len() {
            if other[i..].starts_with(COMMON_NAME.as_bytes()) {
                other[i] = b'D';
            }
        }
        assert!(ShellCert::from_der(&other).is_err());
    }

    #[test]
    fn blob_round_trips() {
        let cert = ShellCert::from_exe(SIGNED).unwrap().unwrap();
        let blob = cert.blob();
        // MS-GPEF: properties, then 0x20 0 0 0 1 0 0 0, length, certificate.
        let tail = [&[0x20, 0, 0, 0, 1, 0, 0, 0][..], &(cert.der.len() as u32).to_le_bytes(), &cert.der].concat();
        assert!(blob.ends_with(&tail));
        assert_eq!(&blob[..8], &[11, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(cert_in_blob(&blob), Some(&cert.der[..]));
        assert_eq!(cert_in_blob(&blob[..blob.len() - 1]), None);
    }

    #[test]
    fn name_encoding() {
        assert_eq!(&our_name()[..9], &[0x30, 0x1d, 0x31, 0x1b, 0x30, 0x19, 0x06, 0x03, 0x55]);
        assert_eq!(tlv(4, &[0; 0x90])[..3], [4, 0x81, 0x90]);
        assert_eq!(tlv(4, &[0; 0x123])[..4], [4, 0x82, 0x01, 0x23]);
    }
}
