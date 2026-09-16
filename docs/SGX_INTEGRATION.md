# SGX Enclave Integration Architecture

## Overview

The oracle node aggregates sensitive off-chain impact data inside an Intel SGX Trusted Execution Environment (TEE). This document describes the enclave integration, remote attestation flow, and sealed storage mechanism.

## Architecture

```text
┌───────────────────────────────────────────────────────────┐
│  Oracle Node (Host / Untrusted)                            │
│  ┌─────────────────────────────────────────────────────┐  │
│  │  pifp-oracle (Rust)                                  │  │
│  │  ┌───────────────────────────────────────────────┐  │  │
│  │  │  AttestationManager                           │  │  │
│  │  │  ├── get_quote() → DCAP quote                 │  │  │
│  │  │  └── verify_peer_quote() → validate peer     │  │  │
│  │  └───────────────────────────────────────────────┘  │  │
│  │  ┌───────────────────────────────────────────────┐  │  │
│  │  │  SealedStorage                                 │  │  │
│  │  │  ├── put(key, plaintext) → seal + write       │  │  │
│  │  │  └── get(key) → read + unseal                 │  │  │
│  │  └───────────────────────────────────────────────┘  │  │
│  │  ┌───────────────────────────────────────────────┐  │  │
│  │  │  EnclaveHandle (ocalls to SGX)                 │  │  │
│  │  └───────────────────────────────────────────────┘  │  │
│  └─────────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────────┘
         │ ocalls │
         ▼        │
┌───────────────────────────────────────────────────────────┐
│  SGX Enclave (Trusted)                                     │
│  ┌─────────────────────────────────────────────────────┐  │
│  │  enclave.signed.so                                  │  │
│  │  ├── aggregate(data) → compute impact score         │  │
│  │  ├── seal(plaintext) → MRENCLAVE-bound ciphertext   │  │
│  │  └── unseal(ciphertext) → recover plaintext         │  │
│  └─────────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────────┘
```

## Enclave Build Configuration

Compile the enclave targeting SGX:

```bash
# Install the SGX SDK and PSW
source /opt/intel/sgxsdk/environment

# Build the enclave
cargo build --target x86_64-fortanix-unknown-sgx --features sgx
```

### Cargo Features

```toml
[features]
default = []
sgx = ["sgx-isa", "sgx-types", "dcap-ql", "aesm-client"]
```

## Remote Attestation (DCAP)

The oracle uses Intel DCAP (Data Center Attestation Primitives) for remote attestation:

1. **Quote Generation:** The enclave calls `sgx_get_quote` via the DCAP Quote Verification Library (QVL).
2. **Collateral Fetching:** The `AttestationManager` fetches the PCK certificate chain from the PCCS (Provisioning Certificate Caching Service).
3. **Quote Verification:** The Soroban contract (or a trusted relayer) verifies the quote signature using the Intel root CA.

### DCAP Flow

```text
Enclave → sgx_get_quote(REPORT) → Quote
Quote → PCCS → PCK Cert Chain
Quote + PCK Chain → QVL → Verification Report
Verification Report → Soroban contract / EVM relayer
```

## Sealed Storage

Sensitive data (API keys, cached impact metrics) is sealed using the SGX sealing key (EGETKEY):

- **MRENCLAVE-bound:** Data can only be unsealed by the exact same enclave build.
- **CPU serial-bound:** Data cannot be migrated to a different physical machine.
- **File format:** `SealedRecord` (ciphertext + tag + nonce + key_id + timestamp).

### File Layout

```
/var/lib/pifp/oracle/sealed/
├── api_keys.sealed
├── impact_metrics.sealed
└── dkg_shares.sealed
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `SGX_ENCLAVE_PATH` | Path to the signed enclave `.so` file |
| `SGX_PCCS_URL` | PCCS endpoint for collateral fetching |
| `SGX_ENFORCE_PRODUCTION` | Reject debug-mode enclaves (`true`/`false`) |
| `SGX_QUOTE_TTL_SECS` | How long to cache a quote before refreshing (default: 3600) |

## Security Considerations

- **Debug mode rejection:** Production deployments must set `SGX_ENFORCE_PRODUCTION=true`.
- **Quote freshness:** Quotes are cached for `SGX_QUOTE_TTL_SECS` to avoid excessive PCCS calls.
- **Sealing key rotation:** Sealed data is lost if the enclave is re-signed with a different key. Plan key rotation carefully.
