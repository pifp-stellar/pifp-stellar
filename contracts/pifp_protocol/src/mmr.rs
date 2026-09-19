//! # Merkle Mountain Range (MMR) for Cross-Chain Impact Proofs
//!
//! Append-only MMR maintained as a peak bag in instance storage.

use soroban_sdk::{
    contract, contractimpl, contracttype, panic_with_error, Address, Bytes, BytesN, Env, Vec,
};

use crate::errors::Error;

const MAX_APPEND_BATCH: u32 = 64;
const MAX_PEAKS: usize = 32;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MmrKey {
    Root,
    LeafCount,
    Peaks(u32),
    Snapshot(u64),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MmrSnapshot {
    pub root: BytesN<32>,
    pub leaf_count: u64,
    pub ledger_timestamp: u64,
    pub project_id: u64,
}

#[contract]
pub struct MmrContract;

#[contractimpl]
impl MmrContract {
    pub fn append(env: Env, caller: Address, leaves: Vec<BytesN<32>>) -> MmrSnapshot {
        caller.require_auth();
        let leaf_count = Self::get_leaf_count(&env);
        let n = leaves.len();
        if n == 0 || n > MAX_APPEND_BATCH {
            panic_with_error!(&env, Error::InvalidMilestones);
        }

        let mut peaks: Vec<BytesN<32>> = Vec::new(&env);
        for h in 0..MAX_PEAKS {
            match env
                .storage()
                .instance()
                .get::<_, BytesN<32>>(&MmrKey::Peaks(h as u32))
            {
                Some(peak) => peaks.push_back(peak),
                None => break,
            }
        }

        for i in 0..n {
            let mut new_peaks = peaks.clone();
            new_peaks.push_back(leaves.get(i).unwrap().clone());
            while new_peaks.len() >= 2 {
                let l = new_peaks.get(0).unwrap().clone();
                let r = new_peaks.get(1).unwrap().clone();
                if l == r {
                    let merged = mmr_combine(&env, &l, &r);
                    new_peaks.remove(0);
                    new_peaks.remove(0);
                    new_peaks.push_back(merged);
                } else {
                    break;
                }
            }
            peaks = new_peaks;
        }

        let new_count = leaf_count + n as u64;
        let root = if peaks.is_empty() {
            BytesN::from_array(&env, &[0u8; 32])
        } else {
            peaks.get(peaks.len() - 1).unwrap().clone()
        };

        env.storage().instance().set(&MmrKey::Root, &root);
        env.storage().instance().set(&MmrKey::LeafCount, &new_count);
        for (h, peak) in peaks.iter().enumerate() {
            if (h as u32) < MAX_PEAKS as u32 {
                env.storage()
                    .instance()
                    .set(&MmrKey::Peaks(h as u32), &peak.clone());
            }
        }
        for h in (peaks.len() as usize)..MAX_PEAKS {
            env.storage().instance().remove(&MmrKey::Peaks(h as u32));
        }

        let snapshot = MmrSnapshot {
            root: root.clone(),
            leaf_count: new_count,
            ledger_timestamp: env.ledger().timestamp(),
            project_id: 0,
        };
        env.storage()
            .persistent()
            .set(&MmrKey::Snapshot(new_count - 1), &snapshot);

        MmrSnapshot {
            root,
            leaf_count: new_count,
            ledger_timestamp: env.ledger().timestamp(),
            project_id: 0,
        }
    }

    pub fn get_root(env: Env) -> BytesN<32> {
        env.storage()
            .instance()
            .get::<_, BytesN<32>>(&MmrKey::Root)
            .unwrap_or_else(|| BytesN::from_array(&env, &[0u8; 32]))
    }

    pub fn get_leaf_count(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&MmrKey::LeafCount)
            .unwrap_or(0)
    }

    pub fn get_snapshot(env: Env, leaf_index: u64) -> Option<MmrSnapshot> {
        env.storage()
            .persistent()
            .get(&MmrKey::Snapshot(leaf_index))
    }
}

fn mmr_combine(env: &Env, left: &BytesN<32>, right: &BytesN<32>) -> BytesN<32> {
    let mut input = Bytes::new(env);
    input.extend_from_slice(b"MMR_PARENT\0");
    input.extend_from_slice(&left.to_array());
    input.extend_from_slice(&right.to_array());
    env.crypto().sha256(&input).into()
}
