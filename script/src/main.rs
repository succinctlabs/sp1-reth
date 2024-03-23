pub mod db;
pub mod init;

use crate::init::SP1RethInputInitializer;
use clap::Parser;
use sp1_core::{utils::BabyBearPoseidon2, SP1Prover, SP1Stdin, SP1Verifier};
use sp1_reth_primitives::SP1RethInput;
use std::fs::File;

/// The version message for the SP1 Reth program.
const VERSION_MESSAGE: &str = concat!(
    "SP1 Reth",
    " (",
    env!("VERGEN_GIT_SHA"),
    " ",
    env!("VERGEN_BUILD_TIMESTAMP"),
    ")"
);

/// The ELF file for the SP1 Reth program.
const SP1_RETH_ELF: &[u8] = include_bytes!("../../program/elf/riscv32im-succinct-zkvm-elf");

/// The CLI arguments for the SP1 Reth program.
#[derive(Parser, Debug)]
#[command(version = VERSION_MESSAGE, about, long_about = None)]
pub struct SP1RethArgs {
    #[arg(short, long)]
    rpc_url: String,

    #[arg(short, long)]
    block_number: u64,

    #[arg(short, long)]
    use_cache: bool,
}

#[tokio::main]
async fn main() {
    // Parse arguments.
    let args = SP1RethArgs::parse();

    // Get input.
    let input: SP1RethInput = if !args.use_cache {
        let input = SP1RethInput::initialize(&args).await.unwrap();
        let mut file =
            File::create(format!("{}.bin", args.block_number)).expect("unable to open file");
        bincode::serialize_into(&mut file, &input).expect("unable to serialize input");
        input
    } else {
        let file = File::open(format!("{}.bin", args.block_number)).expect("unable to open file");
        bincode::deserialize_from(file).expect("unable to deserialize input")
    };

    // Generate proof.
    sp1_core::utils::setup_logger();
    let mut stdin = SP1Stdin::new();
    stdin.write(&input);

    let config = BabyBearPoseidon2::new();
    let proof = SP1Prover::prove_with_config(SP1_RETH_ELF, stdin, config).expect("proving failed");

    // Verify proof.
    let config = BabyBearPoseidon2::new();
    SP1Verifier::verify_with_config(SP1_RETH_ELF, &proof, config).expect("verification failed");

    // Save proof.
    proof
        .save("proof-with-io.json")
        .expect("saving proof failed");

    println!("successfully generated and verified proof for the program!")
}
