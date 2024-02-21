//! A library that can be compiled inside the zkVM for proving the execution of the EVM.

pub mod alloy2reth;
pub mod db;
pub mod mpt;
pub mod processor;

use crate::mpt::MptNode;
use crate::mpt::StorageEntry;

use reth_primitives::{Address, Bytes, Header, TransactionSignedNoHash, Withdrawal, B256};
use revm::primitives::HashMap;
use serde::{Deserialize, Serialize};

/// Necessary information to prove the execution of Ethereum blocks inside SP1.
#[derive(Clone, Serialize, Deserialize)]
pub struct SP1RethInput {
    /// The Keccak 256-bit hash of the parent block's header, in its entirety.
    pub parent_header: Header,

    /// The 160-bit address to which all fees collected from the successful mining of this block
    /// be transferred.
    pub beneficiary: Address,

    /// A scalar value equal to the current limit of gas expenditure per block.
    pub gas_limit: u64,

    /// A scalar value equal to the reasonable output of Unix's time() at this block's inception.
    pub timestamp: u64,

    /// An arbitrary byte array containing data relevant to this block. This must be 32 bytes or
    /// fewer.
    pub extra_data: Bytes,

    /// A 256-bit hash which, combined with the nonce, proves that a sufficient amount of
    /// computation has been carried out on this block.
    pub mix_hash: B256,

    /// The state trie of the parent block.
    pub parent_state_trie: MptNode,

    /// The storage of the parent block.
    pub parent_storage: HashMap<Address, StorageEntry>,

    /// The relevant contracts for the block.
    pub contracts: Vec<Bytes>,

    /// The ancestor headers of the parent block.
    pub ancestor_headers: Vec<Header>,

    /// A list of transactions to process.
    pub transactions: Vec<TransactionSignedNoHash>,

    /// A list of withdrawals to process.
    pub withdrawals: Vec<Withdrawal>,
}
