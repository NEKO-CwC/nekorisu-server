use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use nekorisu::sign::{CONTEXT_CONFIG, SecretKey};

use crate::{Error, Result};

pub struct Signed {
    pub key_id: String,
    pub signature: Vec<u8>,
}

/// Signs configs. The key sits behind this so another signer, such as a
/// KMS, can be used instead of a file.
pub trait Signer: Send + Sync {
    fn key_id(&self) -> String;
    /// The public key, base64, as `nekorisu source add --key` takes it.
    fn public_key(&self) -> String;
    fn sign(&self, payload: &[u8]) -> Signed;
}

/// The config signing key kept in a file in the data directory. It is not the
/// key that signs releases.
pub struct FileSigner {
    key: SecretKey,
}

impl FileSigner {
    /// Loads the key, or makes one on first start. The file is private to the
    /// owner. Returns whether a key was made.
    pub fn load_or_create(path: &Path) -> Result<(Self, bool)> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let key = SecretKey::from_file_json(&text).map_err(|e| Error::Key(e.to_string()))?;
                Ok((Self { key }, false))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let key = SecretKey::generate();
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                write_private(path, key.to_file_json().as_bytes())?;
                Ok((Self { key }, true))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// For tests.
    pub fn generate() -> Self {
        Self {
            key: SecretKey::generate(),
        }
    }
}

#[cfg(unix)]
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(body)
}

#[cfg(not(unix))]
fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, body)
}

impl Signer for FileSigner {
    fn key_id(&self) -> String {
        self.key.public().key_id
    }

    fn public_key(&self) -> String {
        self.key.public().to_base64()
    }

    fn sign(&self, payload: &[u8]) -> Signed {
        let envelope = self.key.sign(CONTEXT_CONFIG, payload);
        Signed {
            key_id: envelope.key_id,
            signature: B64.decode(envelope.signature).expect("the library writes valid base64"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nekorisu::sign::Envelope;

    #[test]
    fn signatures_verify_with_the_client_library() {
        let signer = FileSigner::generate();
        let payload = br#"{"schema":"1.0"}"#;
        let signed = signer.sign(payload);
        // what a device does: build the envelope, open it with the pinned key
        let envelope = Envelope {
            payload: B64.encode(payload),
            key_id: signed.key_id.clone(),
            signature: B64.encode(&signed.signature),
        };
        let key = nekorisu::sign::PublicKey::from_base64(&signer.public_key()).unwrap();
        assert_eq!(key.key_id, signed.key_id);
        assert_eq!(
            envelope.open(CONTEXT_CONFIG, std::slice::from_ref(&key)).unwrap(),
            payload
        );
        // the context keeps a config signature from passing as anything else
        assert!(envelope.open(nekorisu::sign::CONTEXT_RELEASE_MANIFEST, &[key]).is_err());
    }

    #[test]
    fn the_key_file_is_made_once_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/signing-key.json");
        let (first, made) = FileSigner::load_or_create(&path).unwrap();
        assert!(made);
        let (second, made) = FileSigner::load_or_create(&path).unwrap();
        assert!(!made);
        assert_eq!(first.key_id(), second.key_id());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
