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

use alloy_provider::{Provider, RootProvider};
use alloy_rpc_types::{BlockId, EIP1186AccountProofResponse};
use alloy_transport_http::Http;
use anyhow::Result;
use reqwest::Client;
use reth_primitives::revm_primitives::{Account, AccountInfo, Bytecode};
use reth_primitives::{Address, Header, B256, U256};
use revm::db::InMemoryDB;
use revm::primitives::db::Database;
use revm::primitives::HashMap;
use revm::DatabaseCommit;
use sp1_reth_primitives::db::InMemoryDBHelper;
use tokio::runtime::Handle;

/// A database that fetches data from a [HttpProvider].
pub struct RemoteDb {
    /// The provider to fetch data from.
    pub provider: RootProvider<Http<Client>>,

    /// The block number we are executing from.
    pub block_number: u64,

    /// The initial database state.
    pub initial_db: InMemoryDB,

    /// The latest database state.
    pub current_db: InMemoryDB,

    /// An executor for asynchronous tasks, facilitating non-blocking operations.
    async_executor: Handle,
}

impl RemoteDb {
    /// Creates a new provider database from a provider and block number.
    pub fn new(provider: RootProvider<Http<Client>>, block_number: u64) -> Self {
        RemoteDb {
            provider,
            block_number,
            initial_db: InMemoryDB::default(),
            current_db: InMemoryDB::default(),
            async_executor: tokio::runtime::Handle::current(),
        }
    }

    /// Gets all storage proofs for a given block number and a set of storage keys.
    fn fetch_storage_proofs(
        &mut self,
        block_number: u64,
        storage_keys: HashMap<Address, Vec<U256>>,
    ) -> Result<HashMap<Address, EIP1186AccountProofResponse>> {
        let mut storage_proofs = HashMap::new();
        for (address, keys) in storage_keys {
            let indices = keys.into_iter().map(|x| x.to_be_bytes().into()).collect();
            let proof = self.async_executor.block_on(async {
                self.provider
                    .get_proof(address, indices, BlockId::from(block_number))
                    .await
            })?;
            storage_proofs.insert(address, proof);
        }
        Ok(storage_proofs)
    }

    /// Gets all ancestor headers to prove the state transition.
    pub fn fetch_ancestor_headers(&mut self) -> Result<Vec<Header>> {
        let block_number = U256::from(self.block_number);
        let earliest_block = self
            .initial_db
            .block_hashes
            .keys()
            .min()
            .unwrap_or(&block_number);
        let headers = (earliest_block.as_limbs()[0]..self.block_number)
            .rev()
            .map(|block_number| {
                self.async_executor.block_on(async {
                    let header = self
                        .provider
                        .get_block(block_number.into(), false)
                        .await
                        .unwrap()
                        .unwrap()
                        .header;
                    Header {
                        parent_hash: header.parent_hash.0.into(),
                        ommers_hash: header.uncles_hash.0.into(),
                        beneficiary: header.miner.0.into(),
                        state_root: header.state_root.0.into(),
                        transactions_root: header.transactions_root.0.into(),
                        receipts_root: header.receipts_root.0.into(),
                        withdrawals_root: header.withdrawals_root,
                        logs_bloom: header.logs_bloom.0.into(),
                        difficulty: header.difficulty,
                        number: header.number.unwrap(),
                        gas_limit: header.gas_limit.try_into().unwrap(),
                        gas_used: header.gas_used.try_into().unwrap(),
                        timestamp: header.timestamp,
                        extra_data: header.extra_data.0.into(),
                        mix_hash: header.mix_hash.unwrap(),
                        nonce: u64::from_be_bytes(header.nonce.unwrap().0),
                        base_fee_per_gas: Some(
                            header.base_fee_per_gas.unwrap().try_into().unwrap(),
                        ),
                        blob_gas_used: Some(header.blob_gas_used.unwrap().try_into().unwrap()),
                        excess_blob_gas: Some(header.excess_blob_gas.unwrap().try_into().unwrap()),
                        parent_beacon_block_root: header.parent_beacon_block_root,
                    }
                })
            })
            .collect();
        Ok(headers)
    }

    /// Gets the storage proofs for the initial state.
    pub fn fetch_initial_storage_proofs(
        &mut self,
    ) -> Result<HashMap<Address, EIP1186AccountProofResponse>> {
        self.fetch_storage_proofs(self.block_number, self.initial_db.storage_keys())
    }

    /// Gets the storage proofs for the latest state.
    pub fn fetch_latest_storage_proofs(
        &mut self,
    ) -> Result<HashMap<Address, EIP1186AccountProofResponse>> {
        let mut storage_keys = self.initial_db.storage_keys();
        for (address, mut indices) in self.current_db.storage_keys() {
            match storage_keys.get_mut(&address) {
                Some(initial_indices) => initial_indices.append(&mut indices),
                None => {
                    storage_keys.insert(address, indices);
                }
            }
        }
        self.fetch_storage_proofs(self.block_number + 1, storage_keys)
    }
}

impl Database for RemoteDb {
    type Error = anyhow::Error;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        // Check if the account is in the current database.
        if let Ok(db_result) = self.current_db.get_account_info(address) {
            return Ok(db_result);
        }
        if let Ok(db_result) = self.initial_db.get_account_info(address) {
            return Ok(db_result);
        }

        // Get the nonce, balance, and code to reconstruct the account.
        let nonce = self.async_executor.block_on(async {
            self.provider
                .get_transaction_count(address, BlockId::from(self.block_number))
                .await
        })?;
        let balance = self.async_executor.block_on(async {
            self.provider
                .get_balance(address, BlockId::from(self.block_number))
                .await
        })?;
        let code = self.async_executor.block_on(async {
            self.provider
                .get_code_at(address, BlockId::from(self.block_number))
                .await
        })?;

        // Insert the account into the initial database.
        let account_info = AccountInfo::new(
            balance,
            nonce,
            Bytecode::new_raw(code.clone()).hash_slow(),
            Bytecode::new_raw(code),
        );
        self.initial_db
            .insert_account_info(address, account_info.clone());
        Ok(Some(account_info))
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        // Check if the storage slot is in the current database.
        if let Ok(db_result) = self.current_db.get_storage_slot(address, index) {
            return Ok(db_result);
        }
        if let Ok(db_result) = self.initial_db.get_storage_slot(address, index) {
            return Ok(db_result);
        }

        // Get the storage slot from the provider.
        self.initial_db.basic(address)?;
        let storage = self.async_executor.block_on(async {
            self.provider
                .get_storage_at(
                    address.into_array().into(),
                    index,
                    BlockId::from(self.block_number),
                )
                .await
        })?;
        self.initial_db
            .insert_account_storage(address, index, storage)?;
        Ok(storage)
    }

    fn block_hash(&mut self, number: U256) -> Result<B256, Self::Error> {
        // Check if the block hash is in the current database.
        if let Ok(block_hash) = self.initial_db.block_hash(number) {
            return Ok(block_hash);
        }

        // Get the block hash from the provider.
        let block_number = u64::try_from(number).unwrap();
        let block_hash = self.async_executor.block_on(async {
            self.provider
                .get_block_by_number(block_number.into(), false)
                .await
                .unwrap()
                .unwrap()
                .header
                .hash
                .unwrap()
                .0
                .into()
        });
        self.initial_db
            .insert_block_hash(U256::from(block_number), block_hash);
        Ok(block_hash)
    }

    fn code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
        unreachable!()
    }
}

impl DatabaseCommit for RemoteDb {
    fn commit(&mut self, changes: HashMap<Address, Account>) {
        self.current_db.commit(changes)
    }
}
