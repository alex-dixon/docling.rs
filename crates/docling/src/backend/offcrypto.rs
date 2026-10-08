//! Encrypted ("password protected") Office documents — [MS-OFFCRYPTO].
//!
//! Every Office format marks encryption differently: an OOXML package
//! (`.docx`/`.xlsx`/`.pptx`) is no longer a ZIP but a compound file holding
//! `EncryptionInfo` + `EncryptedPackage`; a `.doc` sets `fEncrypted` in its
//! FIB; an `.xls` carries a `FILEPASS` record; a `.ppt` references a
//! `CryptSession10Container` from its current user edit. Each backend checks
//! its own marker and reports the same error (#624), so an encrypted file
//! never converts to an empty document or fails as "bad zip".

use crate::backend::cfb::CompoundFile;
use crate::error::ConversionError;

/// The error for an encrypted `fmt` document.
pub(crate) fn encrypted(fmt: &str) -> ConversionError {
    ConversionError::Parse(format!("{fmt}: document is encrypted"))
}

/// Whether `err` is one of this module's errors — the converter prefers it
/// over the "not a valid <format>" error of a mislabelled file (an
/// encrypted `.ppt` named `.pptx` fails as a ZIP first).
pub(crate) fn is_encryption_error(err: &ConversionError) -> bool {
    matches!(err, ConversionError::Parse(m) if m.ends_with(": document is encrypted"))
}

/// Whether `bytes` is an encrypted OOXML package ([MS-OFFCRYPTO] 2.3.4.4 /
/// 2.3.4.5): a compound file with `EncryptionInfo` and `EncryptedPackage`
/// streams in its root storage. Root only (#512): a document embedding an
/// encrypted one is not itself encrypted.
pub(crate) fn is_encrypted_package(bytes: &[u8]) -> bool {
    if !CompoundFile::detect(bytes) {
        return false;
    }
    let Some(cfb) = CompoundFile::open(bytes) else {
        return false;
    };
    let root: Vec<&str> = cfb
        .children_of(None)
        .into_iter()
        .filter(|&i| !cfb.is_storage(i))
        .map(|i| cfb.entry_name(i))
        .collect();
    root.contains(&"EncryptionInfo") && root.contains(&"EncryptedPackage")
}
