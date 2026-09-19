//! # SGX Sealed Storage
//!
//! Provides encrypted storage for sensitive oracle data (API keys, cached
//! impact metrics) using the SGX enclave's sealing key.  Sealed data can
//! only be read back by the same enclave on the same machine.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::sgx_enclave::{EnclaveHandle, SgxError, SgxResult};

// ─── Sealed Record ───────────────────────────────────────────────────────────

/// A sealed record on disk.  The payload is encrypted with the enclave's
/// sealing key and includes a MAC for integrity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealedRecord {
    /// Encrypted payload ciphertext.
    pub ciphertext: Vec<u8>,
    /// Authentication tag (MAC).
    pub tag: Vec<u8>,
    /// Nonce used for encryption.
    pub nonce: Vec<u8>,
    /// Key identifier (e.g., "api_key", "impact_metrics").
    pub key_id: String,
    /// Ledger timestamp when this record was written.
    pub sealed_at: u64,
}

impl SealedRecord {
    pub fn new(
        ciphertext: Vec<u8>,
        tag: Vec<u8>,
        nonce: Vec<u8>,
        key_id: impl Into<String>,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        Self {
            ciphertext,
            tag,
            nonce,
            key_id: key_id.into(),
            sealed_at: now,
        }
    }
}

// ─── Sealed Storage Engine ───────────────────────────────────────────────────

/// Persistent encrypted storage backed by a local file.
///
/// All reads and writes go through the SGX enclave's seal/unseal path.
/// The underlying file stores serialised `SealedRecord` entries.
pub struct SealedStorage {
    handle: EnclaveHandle,
    path: PathBuf,
    _marker: std::marker::PhantomData<()>,
}

impl SealedStorage {
    /// Open (or create) a sealed storage file at `path`.
    pub fn open(handle: EnclaveHandle, path: impl Into<PathBuf>) -> SgxResult<Self> {
        let path = path.into();
        info!("SealedStorage: opening {}", path.display());

        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(SgxError::Io)?;
        }

        // Create file if it doesn't exist.
        if !path.exists() {
            File::create(&path).map_err(SgxError::Io)?;
        }

        Ok(Self {
            handle,
            path,
            _marker: std::marker::PhantomData,
        })
    }

    /// Seal and persist a sensitive value under `key_id`.
    pub fn put(&self, key_id: impl Into<String>, plaintext: &[u8]) -> SgxResult<()> {
        let key_id = key_id.into();
        debug!("SealedStorage::put key={}", key_id);

        let sealed = self.handle.seal(plaintext)?;
        let record = SealedRecord {
            ciphertext: sealed.clone(),
            tag: vec![0u8; 16],
            nonce: vec![0u8; 12],
            key_id: key_id.clone(),
            sealed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&self.path)
            .map_err(SgxError::Io)?;

        let payload = serde_json::to_vec(&record)
            .map_err(|e| SgxError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;
        let len = payload.len() as u32;
        file.write_all(&len.to_le_bytes())?;
        file.write_all(&payload)?;
        file.flush().map_err(SgxError::Io)?;

        info!(
            "SealedStorage: sealed {} bytes for key={}",
            plaintext.len(),
            key_id
        );
        Ok(())
    }

    /// Read and unseal a value by `key_id`.
    ///
    /// Returns the most recent record matching `key_id`.
    pub fn get(&self, key_id: &str) -> SgxResult<Option<Vec<u8>>> {
        debug!("SealedStorage::get key={}", key_id);

        let mut file = File::open(&self.path).map_err(SgxError::Io)?;
        file.seek(SeekFrom::Start(0)).map_err(SgxError::Io)?;

        let mut latest: Option<Vec<u8>> = None;
        let mut len_buf = [0u8; 4];

        loop {
            match file.read_exact(&mut len_buf) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(SgxError::Io(e)),
            }
            let len = u32::from_le_bytes(len_buf) as usize;
            let mut payload = vec![0u8; len];
            file.read_exact(&mut payload).map_err(SgxError::Io)?;

            let record: SealedRecord = serde_json::from_slice(&payload)
                .map_err(|e| SgxError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;

            if record.key_id == key_id {
                let plaintext = self.handle.unseal(&record.ciphertext)?;
                latest = Some(plaintext);
            }
        }

        info!(
            "SealedStorage: got key={} found={}",
            key_id,
            latest.is_some()
        );
        Ok(latest)
    }

    /// Remove all records matching `key_id`.
    pub fn remove(&self, key_id: &str) -> SgxResult<usize> {
        debug!("SealedStorage::remove key={}", key_id);
        // In production: rewrite the file excluding matching records.
        warn!("SealedStorage::remove is a no-op in the mock implementation");
        Ok(0)
    }

    /// Return the file path for this storage.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
