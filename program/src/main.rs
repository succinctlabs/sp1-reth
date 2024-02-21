//! An implementation of a type-1, bytecompatible compatible, zkEVM written in Rust & SP1.

#![no_main]
sp1_zkvm::entrypoint!(main);

use reth_primitives::B256;
use revm::InMemoryDB;
use sp1_reth_primitives::db::InMemoryDBHelper;
use sp1_reth_primitives::mpt::keccak;
use sp1_reth_primitives::processor::EvmProcessor;
use sp1_reth_primitives::SP1RethInput;

fn main() {
    // Read the input.
    let mut input = sp1_zkvm::io::read::<SP1RethInput>();

    // Initialize the database.
    let db = InMemoryDB::initialize(&mut input).unwrap();

    // Execute the block.
    let mut executor = EvmProcessor::<InMemoryDB> {
        input,
        db: Some(db),
        header: None,
    };
    executor.initialize();
    executor.execute();
    executor.finalize();

    // Print the resulting block hash.
    let hash = B256::from(keccak(alloy_rlp::encode(executor.header.unwrap())));
    println!("block hash: {}", hash);
}
