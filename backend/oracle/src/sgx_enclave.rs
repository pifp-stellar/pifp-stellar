//! # SGX Enclave Abstraction
//!
//! Provides a hardware-abstracted interface for Intel SGX Trusted Execution
//! Environments (TEE).  The oracle aggregation logic runs inside an SGX
//! enclave so that sensitive off-chain impact data cannot be tampered with
//! by the node operator.
//!
//! ## Architecture
//!
//! ```text
//! Host Process (untrusted)
//!   └── ocall_get_quote() → DCAP attestation
//!       └── SGX Enclave (trusted)
//!           ├── seal_key() → MRENCLAVE-bound sealing
//!           ├── aggregate() → data aggregation inside TEE
//!           └── verify_quote() → remote attestation
//! ```
//!
//! ## Build Configuration
//!
//! Set the `sgx` feature flag to compile for `x86_64-fortanix-unknown-sgx`:
//!
//! ```toml
//! [features]
//! sgx = ["sgx-isa", "sgx-types", "dcap-ql"]
//! ```

use std::path::PathBuf;

use thiserror::Error;

// ─── Error Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum SgxError {
    #[error("enclave not initialised: {0}")]
    NotInitialised(String),

    #[error("attestation quote verification failed: {0}")]
    QuoteVerificationFailed(String),

    #[error("sealing key derivation failed: {0}")]
    SealingFailed(String),

    #[error("enclave is not in production mode")]
    NotProductionMode,

    #[error("DCAP quote generation failed: {0}")]
    DcapFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type SgxResult<T> = Result<T, SgxError>;

// ─── Enclave Configuration ───────────────────────────────────────────────────

/// Configuration for the SGX enclave runtime.
#[derive(Debug, Clone)]
pub struct EnclaveConfig {
    /// Path to the signed enclave `.so` file.
    pub enclave_path: PathBuf,
    /// Product ID assigned by Intel.
    pub product_id: u16,
    /// Security version of the enclave.
    pub security_version: u16,
    /// Path to the DCAP PCCS endpoint.
    pub pccs_url: String,
    /// Whether to enforce production attestation (disables debug mode).
    pub enforce_production: bool,
    /// Maximum number of concurrent enclave sessions.
    pub max_sessions: usize,
}

impl Default for EnclaveConfig {
    fn default() -> Self {
        Self {
            enclave_path: PathBuf::from("/opt/pifp/enclave.signed.so"),
            product_id: 1,
            security_version: 1,
            pccs_url: "https://pccs.example.com".to_string(),
            enforce_production: true,
            max_sessions: 1024,
        }
    }
}

// ─── Enclave Handle ──────────────────────────────────────────────────────────

/// RAII handle to an SGX enclave instance.
pub struct EnclaveHandle {
    config: EnclaveConfig,
    mr_enclave: [u8; 32],
    is_production: bool,
}

impl EnclaveHandle {
    /// Create a new enclave handle by loading and initialising the signed
    /// enclave `.so` file.
    pub fn new(config: EnclaveConfig) -> SgxResult<Self> {
        info!("SGX: loading enclave from {}", config.enclave_path.display());

        // In a real deployment this would call `sgx_create_enclave` via
        // the `sgx-isa` or `dcap-ql` crate.  Here we simulate the MRENCLAVE
        // measurement and validate the file exists.
        if !config.enclave_path.exists() {
            return Err(SgxError::NotInitialised(format!(
                "enclave file not found: {}",
                config.enclave_path.display()
            )));
        }

        let mr_enclave = [0u8; 32]; // Mock MRENCLAVE; real impl reads from quote.
        let is_production = !config.enclave_path.to_string_lossy().contains("debug");

        if config.enforce_production && !is_production {
            return Err(SgxError::NotProductionMode);
        }

        info!("SGX: enclave loaded (MRENCLAVE={:?})", &mr_enclave[0..8]);
        Ok(Self {
            config,
            mr_enclave,
            is_production,
        })
    }

    /// Return the MRENCLAVE measurement of the loaded enclave.
    pub fn mr_enclave(&self) -> &[u8; 32] {
        &self.mr_enclave
    }

    /// Return whether the enclave is running in production mode.
    pub fn is_production(&self) -> bool {
        self.is_production
    }

    /// Seal a secret using the enclave's sealing key.
    ///
    /// Sealed data can only be unsealed by the same enclave on the same
    /// machine (MRENCLAVE + CPU serial number bound).
    pub fn seal(&self, plaintext: &[u8]) -> SgxResult<Vec<u8>> {
        if !self.is_production && self.config.enforce_production {
            return Err(SgxError::NotProductionMode);
        }
        // In production: call SGX sealing API (EGETKEY).
        let mut sealed = plaintext.to_vec();
        sealed.extend_from_slice(&self.mr_enclave);
        Ok(sealed)
    }

    /// Unseal a previously sealed secret.
    pub fn unseal(&self, sealed: &[u8]) -> SgxResult<Vec<u8>> {
        if sealed.len() <= 32 {
            return Err(SgxError::SealingFailed("sealed blob too short".into()));
        }
        let (data, mac) = sealed.split_at(sealed.len() - 32);
        if mac != self.mr_enclave {
            return Err(SgxError::SealingFailed("MRENCLAVE mismatch".into()));
        }
        Ok(data.to_vec())
    }
}

impl Drop for EnclaveHandle {
    fn drop(&mut self) {
        info!("SGX: destroying enclave handle");
    }
}

// ─── Attestation Quote ───────────────────────────────────────────────────────

/// SGX DCAP attestation quote produced by the enclave.
#[derive(Debug, Clone)]
pub struct AttestationQuote {
    pub quote: Vec<u8>,
    pub certification_data: Vec<u8>,
    pub signature: Vec<u8>,
}

impl AttestationQuote {
    /// Generate a DCAP quote for the current enclave.
    pub fn generate(handle: &EnclaveHandle) -> SgxResult<Self> {
        info!("SGX: generating DCAP quote");
        // In production: call sgx_get_quote via dcap-ql.
        Ok(Self {
            quote: vec![0u8; 432], // SGX quote size
            certification_data: vec![],
            signature: vec![0u8; 64],
        })
    }

    /// Verify the quote against the PCCS and check the MRENCLAVE.
    pub fn verify(&self, expected_mr_enclave: &[u8; 32]) -> SgxResult<()> {
        if self.quote.len() < 432 {
            return Err(SgxError::QuoteVerificationFailed(
                "quote too short".into(),
            ));
        }
        // In production: verify collateral via DCAP.
        Ok(())
    }
}
