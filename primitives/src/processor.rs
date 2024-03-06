// This code is modified from the original implementation of Zeth.
//
// Reference: https://github.com/risc0/zeth
//
// Copyright 2023 RISC Zero, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::mpt::keccak;
use crate::mpt::RlpBytes;
use crate::mpt::StateAccount;
use crate::SP1RethInput;

use anyhow::anyhow;
use reth_primitives::proofs::ordered_trie_root_with_encoder;
use reth_primitives::revm_primitives::Account;
use reth_primitives::{Address, Bloom, Transaction, TransactionKind, TransactionSigned};
use reth_primitives::{BaseFeeParams, Receipt, ReceiptWithBloom};
use reth_primitives::{Header, U256};
use revm::db::AccountState;
use revm::db::InMemoryDB;
use revm::interpreter::Host;
use revm::primitives::{SpecId, TransactTo, TxEnv};
use revm::{Database, DatabaseCommit, Evm};
use std::mem;
use std::mem::take;

/// The divisor for the gas limit bound.
///
/// Reference: https://github.com/paradigmxyz/reth/blob/main/crates/primitives/src/header.rs#L752
pub const GAS_LIMIT_DIVISOR: u64 = 1024;

/// The minimum gas limit.
///
/// Reference: https://github.com/paradigmxyz/reth/blob/main/crates/primitives/src/constants/mod.rs#L65
pub const MINIMUM_GAS_LIMIT: u64 = 5000;

/// The maximum number of extra data bytes.
///
/// Reference: https://github.com/paradigmxyz/reth/blob/main/crates/primitives/src/constants/mod.rs#L19
pub const MAXIMUM_EXTRA_DATA_SIZE: usize = 32;

/// A processor that executes EVM transactions.
pub struct EvmProcessor<D> {
    /// An input containing all necessary data to execute the block.
    pub input: SP1RethInput,

    /// A database to store all state changes.
    pub db: Option<D>,

    /// The header to be finalized.
    pub header: Option<Header>,
}

impl<D> EvmProcessor<D> {
    /// Validate the header standalone.
    ///
    /// Reference: https://github.com/paradigmxyz/reth/blob/main/crates/consensus/common/src/validation.rs#L14
    pub fn validate_header_standalone(&self) {
        let header = self.header.as_ref().unwrap();

        // Gas used needs to be less then gas limit. Gas used is going to be check after execution.
        if header.gas_used > header.gas_limit {
            panic!("Gas used exceeds gas limit");
        }
    }

    /// Validates the integrity and consistency of a block header in relation to it's parent.
    ///
    /// Reference: https://github.com/paradigmxyz/reth/blob/main/crates/primitives/src/header.rs#L800
    pub fn validate_against_parent(&self) {
        let parent_header = &self.input.parent_header;
        let header = self.header.as_ref().unwrap();

        // Parent number is consistent.
        if parent_header.number + 1 != header.number {
            panic!("Parent number is inconsistent with header number");
        }

        // Parent hash is consistent.
        if parent_header.hash_slow() != header.parent_hash {
            panic!("Parent hash is inconsistent with header parent hash");
        }

        // Timestamp in past check.
        if parent_header.timestamp > header.timestamp {
            panic!("Timestamp is in the future");
        }
    }

    /// Checks the gas limit for consistency between parent and self headers.
    ///
    /// Reference: https://github.com/paradigmxyz/reth/blob/main/crates/primitives/src/header.rs#L738
    pub fn validate_gas_limit(&self) {
        let parent_header = &self.input.parent_header;
        let header = self.header.as_ref().unwrap();
        let parent_gas_limit = parent_header.gas_limit;

        // Check for an increase in gas limit beyond the allowed threshold.
        if header.gas_limit > parent_gas_limit {
            if header.gas_limit - parent_gas_limit >= parent_gas_limit / 1024 {
                panic!("Gas limit invalid increase");
            }
        }
        // Check for a decrease in gas limit beyond the allowed threshold.
        else if parent_gas_limit - header.gas_limit >= parent_gas_limit / 1024 {
            panic!("Gas limit invalid decrease");
        }
        // Check if the self gas limit is below the minimum required limit.
        else if parent_gas_limit < MINIMUM_GAS_LIMIT {
            panic!("Gas limit below minimum");
        }
    }

    /// Validates the header's extradata according to the beacon consensus rules.
    ///
    /// Reference: https://github.com/paradigmxyz/reth/blob/main/crates/consensus/beacon-core/src/lib.rs#L118
    pub fn validate_header_extradata(&self) {
        let header = self.header.as_ref().unwrap();
        if header.extra_data.len() > MAXIMUM_EXTRA_DATA_SIZE {
            panic!("Extra data too large");
        }
    }
}

impl<D: Database + DatabaseCommit> EvmProcessor<D>
where
    <D as Database>::Error: core::fmt::Debug,
{
    /// Validate input values against the parent header and initialize the current header's
    /// computed fields.
    pub fn initialize(&mut self) {
        let params = BaseFeeParams::ethereum();
        let base_fee = self.input.parent_header.next_block_base_fee(params);
        let header = Header {
            parent_hash: self.input.parent_header.hash_slow(),
            number: self.input.parent_header.number.checked_add(1).unwrap(),
            base_fee_per_gas: base_fee,
            beneficiary: self.input.beneficiary,
            gas_limit: self.input.gas_limit,
            timestamp: self.input.timestamp,
            mix_hash: self.input.mix_hash,
            extra_data: self.input.extra_data.clone(),
            ..Default::default()
        };
        self.header = Some(header);
        self.validate_against_parent();
        self.validate_header_extradata();
    }

    /// Processes each transaction and collect receipts and storage changes.
    pub fn execute(&mut self) {
        let gwei_to_wei: U256 = U256::from(1_000_000_000);
        let spec_id = SpecId::SHANGHAI;
        let mut evm = Evm::builder()
            .with_spec_id(spec_id)
            .modify_cfg_env(|cfg_env| {
                cfg_env.chain_id = 1;
            })
            .modify_block_env(|blk_env| {
                blk_env.number = self.header.as_mut().unwrap().number.try_into().unwrap();
                blk_env.coinbase = self.input.beneficiary;
                blk_env.timestamp = U256::from(self.header.as_mut().unwrap().timestamp);
                blk_env.difficulty = U256::ZERO;
                blk_env.prevrandao = Some(self.header.as_mut().unwrap().mix_hash);
                blk_env.basefee =
                    U256::from(self.header.as_mut().unwrap().base_fee_per_gas.unwrap());
                blk_env.gas_limit = U256::from(self.header.as_mut().unwrap().gas_limit);
            })
            .with_db(self.db.take().unwrap())
            .build();

        let mut logs_bloom = Bloom::default();
        let mut cumulative_gas_used = U256::ZERO;
        let mut receipts = Vec::new();

        for (tx_no, tx) in self.input.transactions.iter().enumerate() {
            // Recover the sender from the transaction signature.
            let tx_from = tx.recover_signer().unwrap();

            // Validate tx gas.
            let block_available_gas = U256::from(self.input.gas_limit) - cumulative_gas_used;
            if block_available_gas < U256::from(tx.transaction.gas_limit()) {
                panic!("Error at transaction {}: gas exceeds block limit", tx_no);
            }

            // Setup EVM from tx.
            fill_eth_tx_env(&mut evm.env_mut().tx, &tx.transaction, tx_from);
            // Execute transaction.
            let res = evm
                .transact()
                .map_err(|e| {
                    println!("Error at transaction {}: {:?}", tx_no, e);
                    e
                })
                .unwrap();

            // Update cumulative gas used.
            let gas_used = res.result.gas_used().try_into().unwrap();
            cumulative_gas_used = cumulative_gas_used.checked_add(gas_used).unwrap();

            // Create receipt.
            let receipt = Receipt {
                tx_type: tx.transaction.tx_type(),
                success: res.result.is_success(),
                cumulative_gas_used: cumulative_gas_used.try_into().unwrap(),
                logs: res
                    .result
                    .logs()
                    .into_iter()
                    .map(|log| log.into())
                    .collect(),
            };

            // Update logs bloom.
            logs_bloom.accrue_bloom(&receipt.bloom_slow());
            let receipt = ReceiptWithBloom::from(receipt);
            receipts.push(receipt);

            // Commit state changes.
            evm.context.evm.db.commit(res.state);
        }

        // Process consensus layer withdrawals.
        for withdrawal in self.input.withdrawals.iter() {
            // Convert withdrawal amount (in gwei) to wei.
            let amount_wei = gwei_to_wei
                .checked_mul(withdrawal.amount.try_into().unwrap())
                .unwrap();

            increase_account_balance(&mut evm.context.evm.db, withdrawal.address, amount_wei)
                .unwrap();
        }

        // Compute header roots and fill out other header fields.
        let h = self.header.as_mut().expect("Header not initialized");
        let txs_signed = take(&mut self.input.transactions)
            .into_iter()
            .map(|tx| tx.into())
            .collect::<Vec<TransactionSigned>>();
        h.transactions_root = ordered_trie_root_with_encoder(&txs_signed, |tx, buf| {
            tx.encode_with_signature(&tx.signature, buf, false);
        });
        h.receipts_root = ordered_trie_root_with_encoder(&receipts, |receipt, buf| {
            receipt.encode_inner(buf, false);
        });
        h.withdrawals_root = Some(ordered_trie_root_with_encoder(
            &self.input.withdrawals,
            |withdrawal, buf| buf.put_slice(&withdrawal.to_rlp()),
        ));
        h.logs_bloom = logs_bloom;
        h.gas_used = cumulative_gas_used.try_into().unwrap();

        self.db = Some(evm.context.evm.db);
    }
}

impl EvmProcessor<InMemoryDB> {
    /// Process all state changes and finalize the header's state root.
    pub fn finalize(&mut self) {
        let db = self.db.take().expect("DB not initialized");

        let mut state_trie = mem::take(&mut self.input.parent_state_trie);
        for (address, account) in &db.accounts {
            // Ignore untouched accounts.
            if account.account_state == AccountState::None {
                continue;
            }

            let state_trie_index = keccak(address);

            // Remove from state trie if it has been deleted.
            if account.account_state == AccountState::NotExisting {
                state_trie.delete(&state_trie_index).unwrap();
                continue;
            }

            // Update storage root for account.
            let state_storage = &account.storage;
            let storage_root = {
                let (storage_trie, _) = self.input.parent_storage.get_mut(address).unwrap();
                // If the account has been cleared, clear the storage trie.
                if account.account_state == AccountState::StorageCleared {
                    storage_trie.clear();
                }

                // Apply all storage changes to the storage trie.
                for (key, value) in state_storage {
                    let storage_trie_index = keccak(key.to_be_bytes::<32>());
                    if value == &U256::ZERO {
                        storage_trie.delete(&storage_trie_index).unwrap();
                    } else {
                        storage_trie
                            .insert_rlp(&storage_trie_index, *value)
                            .unwrap();
                    }
                }

                storage_trie.hash()
            };

            let state_account = StateAccount {
                nonce: account.info.nonce,
                balance: account.info.balance,
                storage_root,
                code_hash: account.info.code_hash,
            };
            state_trie
                .insert_rlp(&state_trie_index, state_account)
                .unwrap();
        }

        // Update state trie root in header.
        let header = self.header.as_mut().expect("Header not initialized");
        header.state_root = state_trie.hash();

        println!("{:?}", header);
    }
}

fn fill_eth_tx_env(tx_env: &mut TxEnv, essence: &Transaction, caller: Address) {
    match essence {
        Transaction::Legacy(tx) => {
            tx_env.caller = caller;
            tx_env.gas_limit = tx.gas_limit;
            tx_env.gas_price = U256::from(tx.gas_price);
            tx_env.gas_priority_fee = None;
            tx_env.transact_to = if let TransactionKind::Call(to_addr) = tx.to {
                TransactTo::Call(to_addr)
            } else {
                TransactTo::create()
            };
            tx_env.value = tx.value.into();
            tx_env.data = tx.input.clone();
            tx_env.chain_id = tx.chain_id;
            tx_env.nonce = Some(tx.nonce);
            tx_env.access_list.clear();
        }
        Transaction::Eip2930(tx) => {
            tx_env.caller = caller;
            tx_env.gas_limit = tx.gas_limit;
            tx_env.gas_price = U256::from(tx.gas_price);
            tx_env.gas_priority_fee = None;
            tx_env.transact_to = if let TransactionKind::Call(to_addr) = tx.to {
                TransactTo::Call(to_addr)
            } else {
                TransactTo::create()
            };
            tx_env.value = tx.value.into();
            tx_env.data = tx.input.clone();
            tx_env.chain_id = Some(tx.chain_id);
            tx_env.nonce = Some(tx.nonce);
            tx_env.access_list = tx
                .access_list
                .0
                .iter()
                .map(|item| {
                    (
                        item.address,
                        item.storage_keys.iter().map(|key| (*key).into()).collect(),
                    )
                })
                .collect();
        }
        Transaction::Eip1559(tx) => {
            tx_env.caller = caller;
            tx_env.gas_limit = tx.gas_limit;
            tx_env.gas_price = U256::from(tx.max_fee_per_gas);
            tx_env.gas_priority_fee = Some(U256::from(tx.max_priority_fee_per_gas));
            tx_env.transact_to = if let TransactionKind::Call(to_addr) = tx.to {
                TransactTo::Call(to_addr)
            } else {
                TransactTo::create()
            };
            tx_env.value = tx.value.into();
            tx_env.data = tx.input.clone();
            tx_env.chain_id = Some(tx.chain_id);
            tx_env.nonce = Some(tx.nonce);
            tx_env.access_list = tx
                .access_list
                .0
                .iter()
                .map(|item| {
                    (
                        item.address,
                        item.storage_keys.iter().map(|key| (*key).into()).collect(),
                    )
                })
                .collect();
        }
        Transaction::Eip4844(_) => todo!(),
    };
}

pub fn increase_account_balance<D>(
    db: &mut D,
    address: Address,
    amount_wei: U256,
) -> anyhow::Result<()>
where
    D: Database + DatabaseCommit,
    <D as Database>::Error: core::fmt::Debug,
{
    // Read account from database
    let mut account: Account = db
        .basic(address)
        .map_err(|db_err| {
            anyhow!(
                "Error increasing account balance for {}: {:?}",
                address,
                db_err
            )
        })?
        .unwrap_or_default()
        .into();
    // Credit withdrawal amount
    account.info.balance = account.info.balance.checked_add(amount_wei).unwrap();
    account.mark_touch();
    // Commit changes to database
    db.commit([(address, account)].into());

    Ok(())
}
