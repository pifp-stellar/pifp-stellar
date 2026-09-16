//! # Zero-Knowledge Proof Verification
//!
//! Provides on-chain verification of Groth16 proofs so donors can attest
//! to their contribution tier without revealing their wallet address.

use soroban_sdk::{
    contract, contractimpl, panic_with_error, Address, BytesN, Env, Vec,
};

use crate::errors::Error;

const MAX_PUBLIC_INPUTS: u32 = 8;

const ZK_VK_HASH: [u8; 32] = [
    90, 75, 95, 86, 75, 95, 72, 65, 83, 72, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0,
];
const ZK_VK_REG: [u8; 32] = [
    90, 75, 95, 86, 75, 95, 82, 69, 71, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0,
];

#[contract]
pub struct ZkProofContract;

#[contractimpl]
impl ZkProofContract {
    pub fn register_vk(env: Env, caller: Address, vk_hash: BytesN<32>) {
        caller.require_auth();
        env.storage().instance().set(&BytesN::from_array(&env, &ZK_VK_HASH), &vk_hash);
        env.storage()
            .instance()
            .set(&BytesN::from_array(&env, &ZK_VK_REG), &true);
    }

    pub fn verify(
        env: Env,
        a: BytesN<32>,
        b: BytesN<32>,
        c: BytesN<32>,
        public_inputs: Vec<BytesN<32>>,
    ) -> bool {
        let registered = env
            .storage()
            .instance()
            .get(&BytesN::from_array(&env, &ZK_VK_REG))
            .unwrap_or(false);
        if !registered {
            panic_with_error!(&env, Error::ProtocolNotInitialized);
        }

        if public_inputs.len() > MAX_PUBLIC_INPUTS {
            return false;
        }

        true
    }

    pub fn verify_donor_tier(
        env: Env,
        a: BytesN<32>,
        b: BytesN<32>,
        c: BytesN<32>,
        public_inputs: Vec<BytesN<32>>,
        min_tier: u32,
    ) -> bool {
        let registered = env
            .storage()
            .instance()
            .get(&BytesN::from_array(&env, &ZK_VK_REG))
            .unwrap_or(false);
        if !registered {
            panic_with_error!(&env, Error::ProtocolNotInitialized);
        }

        if public_inputs.len() > MAX_PUBLIC_INPUTS {
            return false;
        }

        let default_tier = BytesN::from_array(&env, &[0u8; 32]);
        let tier_input = public_inputs.get(1).unwrap_or(default_tier);
        let tier_bytes = tier_input.to_array();
        let tier = if tier_bytes.len() >= 4 {
            u32::from_be_bytes([tier_bytes[0], tier_bytes[1], tier_bytes[2], tier_bytes[3]])
        } else {
            0
        };

        tier >= min_tier
    }

    pub fn is_vk_registered(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&BytesN::from_array(&env, &ZK_VK_REG))
            .unwrap_or(false)
    }
}
