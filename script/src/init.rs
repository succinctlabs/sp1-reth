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

use crate::db::RemoteDb;
use crate::SP1RethArgs;
use alloy_providers::provider::HttpProvider;
use alloy_providers::provider::TempProvider;
use alloy_rpc_types::BlockTransactions;
use alloy_transport_http::Http;
use anyhow::Result;
use async_trait::async_trait;
use reth_primitives::Bytes;
use sp1_reth_primitives::alloy2reth::IntoReth;
use sp1_reth_primitives::mpt::proofs_to_tries;
use sp1_reth_primitives::processor::EvmProcessor;
use sp1_reth_primitives::SP1RethInput;
use std::collections::HashSet;
use url::Url;

#[async_trait]
pub trait SP1RethInputInitializer {
    /// Initialize [SP1RethInput] from [SP1RethArgs].
    async fn initialize(args: &SP1RethArgs) -> Result<Self>
    where
        Self: Sized;
}

#[async_trait]
impl SP1RethInputInitializer for SP1RethInput {
    async fn initialize(args: &SP1RethArgs) -> Result<Self> {
        // Initialize the provider.
        let http = Http::new(Url::parse(&args.rpc_url).expect("invalid rpc url"));
        let provider: HttpProvider = HttpProvider::new(http);

        // Get the block.
        let parent_block = provider
            .get_block_by_number((args.block_number - 1).into(), false)
            .await?;
        let parent_header = parent_block.unwrap().header;
        let block = provider
            .get_block_by_number(args.block_number.into(), true)
            .await?
            .unwrap();

        // Intiialize the db.
        let provider_db = RemoteDb::new(provider, parent_header.number.unwrap().as_limbs()[0]);

        // Create the input.
        let txs = match block.transactions {
            BlockTransactions::Full(txs) => txs.into_iter().map(|tx| tx.into_reth()).collect(),
            _ => unreachable!(),
        };
        let withdrawals = block
            .withdrawals
            .unwrap()
            .into_iter()
            .map(|w| w.into_reth())
            .collect();
        let input = SP1RethInput {
            beneficiary: block.header.miner,
            gas_limit: block.header.gas_limit.try_into().unwrap(),
            timestamp: block.header.timestamp.try_into().unwrap(),
            extra_data: block.header.extra_data,
            mix_hash: block.header.mix_hash.unwrap(),
            transactions: txs,
            withdrawals,
            parent_state_trie: Default::default(),
            parent_storage: Default::default(),
            contracts: Default::default(),
            parent_header: parent_header.into_reth(),
            ancestor_headers: Default::default(),
        };

        let mut executor = EvmProcessor::<RemoteDb> {
            input: input.clone(),
            db: Some(provider_db),
            header: None,
        };
        executor.initialize();
        let mut executor = tokio::task::spawn_blocking(move || {
            executor.execute();
            executor
        })
        .await?;

        // Get the proofs and ancestor headers.
        let mut provider_db = executor.db.take().unwrap();
        let (parent_proofs, proofs, ancestor_headers, provider_db) =
            tokio::task::spawn_blocking(move || {
                let parent_proofs = provider_db.fetch_initial_storage_proofs().unwrap();
                let proofs = provider_db.fetch_latest_storage_proofs().unwrap();
                let ancestor_headers = provider_db.fetch_ancestor_headers().unwrap();
                (parent_proofs, proofs, ancestor_headers, provider_db)
            })
            .await?;

        // Get the contracts from the initial db.
        let mut contracts = HashSet::new();
        let initial_db = provider_db.initial_db;
        for account in initial_db.accounts.values() {
            let code = &account.info.code;
            if let Some(code) = code {
                contracts.insert(code.bytecode.0.clone());
            }
        }

        // Construct the state trie and storage from the proofs.
        let (state_trie, storage) =
            proofs_to_tries(input.parent_header.state_root, parent_proofs, proofs)?;

        // Create the block builder input
        let input = SP1RethInput {
            parent_state_trie: state_trie,
            parent_storage: storage,
            contracts: contracts.into_iter().map(Bytes).collect(),
            ancestor_headers,
            ..input
        };

        // DONE!

        Ok(input)
    }
}
