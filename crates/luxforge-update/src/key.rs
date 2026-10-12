//! Ed25519 keys and signatures in Sparkle's form, through `ring`.
//!
//! A signature is Sparkle's `sparkle:edSignature`: standard base64 (with padding) of the 64-byte
//! Ed25519 signature over the whole archive. A public key is `SUPublicEDKey`: base64 of its 32
//! bytes. A private key is what Sparkle 2's `generate_keys -x` exports: base64 of the 32-byte seed,
//! surrounding whitespace ignored. Ed25519 is deterministic, so a signature made here equals
//! `sign_update`'s for the same key and file.
//!
//! Files are signed and verified through a read-only memory map, so an archive of any size is not
//! read onto the heap.
use crate::error::{Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use memmap2::Mmap;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use std::{fmt, fs::File, path::Path};
use zeroize::Zeroizing;

/// The longest private key text read: a 32-byte seed is 44 base64 characters.
pub const MAX_PRIVATE_KEY_TEXT: usize = 4096;

/// An Ed25519 public key, `SUPublicEDKey`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    pub fn from_base64(text: &str) -> Result<PublicKey> {
        decode_exact(text.trim())
            .map(PublicKey)
            .ok_or(Error::MalformedPublicKey)
    }

    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Whether `signature` is this key's signature of `bytes`.
    pub fn verify(&self, bytes: &[u8], signature: &Signature) -> Result<()> {
        UnparsedPublicKey::new(&ED25519, self.0)
            .verify(bytes, &signature.0)
            .map_err(|_| Error::BadSignature)
    }

    /// Whether `signature` is this key's signature of the file at `path`, which must be
    /// `expected_length` bytes long. The length is checked before the file is mapped, so a file
    /// longer than its feed item states is refused without being read.
    pub fn verify_file(
        &self,
        path: &Path,
        expected_length: u64,
        signature: &Signature,
    ) -> Result<()> {
        let file = File::open(path)?;
        let actual = file.metadata()?.len();
        if actual != expected_length {
            return Err(Error::LengthMismatch {
                expected: expected_length,
                actual,
            });
        }
        with_mapped(&file, actual, |bytes| self.verify(bytes, signature))?
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_base64())
    }
}

impl TryFrom<String> for PublicKey {
    type Error = Error;
    fn try_from(text: String) -> Result<PublicKey> {
        PublicKey::from_base64(&text)
    }
}

impl From<PublicKey> for String {
    fn from(key: PublicKey) -> String {
        key.to_base64()
    }
}

/// An Ed25519 signature, `sparkle:edSignature`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Signature([u8; 64]);

impl Signature {
    pub fn from_base64(text: &str) -> Result<Signature> {
        decode_exact(text.trim())
            .map(Signature)
            .ok_or(Error::MalformedSignature)
    }

    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({})", self.to_base64())
    }
}

impl TryFrom<String> for Signature {
    type Error = Error;
    fn try_from(text: String) -> Result<Signature> {
        Signature::from_base64(&text)
    }
}

impl From<Signature> for String {
    fn from(signature: Signature) -> String {
        signature.to_base64()
    }
}

/// A private Ed25519 key. Its seed is zeroed when it is decoded; neither `Debug` nor any error
/// shows it.
pub struct SigningKey(Ed25519KeyPair);

impl SigningKey {
    /// A key from the text `generate_keys -x` exports: base64 of the 32-byte seed. Sparkle's
    /// older 96-byte form is refused as [`Error::LegacyKey`]; anything else as
    /// [`Error::MalformedKey`].
    pub fn from_sparkle_export(text: &str) -> Result<SigningKey> {
        let text = text.trim();
        if text.len() > MAX_PRIVATE_KEY_TEXT {
            return Err(Error::MalformedKey("longer than any exported key"));
        }
        let mut decoded = Zeroizing::new(Vec::with_capacity(text.len()));
        STANDARD
            .decode_vec(text, &mut decoded)
            .map_err(|_| Error::MalformedKey("not standard base64"))?;
        match decoded.len() {
            32 => SigningKey::from_seed(&decoded),
            96 => Err(Error::LegacyKey),
            _ => Err(Error::MalformedKey("not 32 bytes once decoded")),
        }
    }

    /// A key from its 32-byte seed.
    pub fn from_seed(seed: &[u8]) -> Result<SigningKey> {
        Ed25519KeyPair::from_seed_unchecked(seed)
            .map(SigningKey)
            .map_err(|_| Error::MalformedKey("not a 32-byte Ed25519 seed"))
    }

    pub fn public_key(&self) -> PublicKey {
        let mut bytes = [0; 32];
        bytes.copy_from_slice(self.0.public_key().as_ref());
        PublicKey(bytes)
    }

    pub fn sign(&self, bytes: &[u8]) -> Signature {
        let mut signature = [0; 64];
        signature.copy_from_slice(self.0.sign(bytes).as_ref());
        Signature(signature)
    }

    /// The signature of the file at `path` and its length.
    pub fn sign_file(&self, path: &Path) -> Result<(Signature, u64)> {
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        let signature = with_mapped(&file, length, |bytes| self.sign(bytes))?;
        Ok((signature, length))
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SigningKey(public {})", self.public_key().to_base64())
    }
}

fn decode_exact<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() > N.div_ceil(3) * 4 {
        return None;
    }
    STANDARD.decode(text).ok()?.try_into().ok()
}

/// Calls `with` on the whole of `file`, `length` bytes as its metadata reported, through a
/// read-only memory map.
fn with_mapped<T>(file: &File, length: u64, with: impl FnOnce(&[u8]) -> T) -> Result<T> {
    if length == 0 {
        // An empty file cannot be mapped on every platform.
        return Ok(with(&[]));
    }
    let map = map(file)?;
    if map.len() as u64 != length {
        return Err(Error::LengthMismatch {
            expected: length,
            actual: map.len() as u64,
        });
    }
    #[cfg(unix)]
    map.advise(memmap2::Advice::Sequential)?;
    Ok(with(&map))
}

#[allow(unsafe_code)]
fn map(file: &File) -> std::io::Result<Mmap> {
    // SAFETY: the map is read-only and lives only for one signing or verification. Were another
    // process to rewrite the file meanwhile its bytes would change under the borrow, and were it
    // to truncate the file the read would fault; the files mapped are archives in a build output
    // or the updater's own staging directory, which nothing else writes while they are mapped.
    unsafe { Mmap::map(file) }
}
