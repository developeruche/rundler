use ethers::types::Chain;

pub const OP_BEDROCK_CHAIN_IDS: &[u64] = &[
    Chain::Optimism as u64,
    Chain::OptimismGoerli as u64,
    8453, // Base
    Chain::BaseGoerli as u64,
];

pub const ARBITRUM_CHAIN_IDS: &[u64] = &[Chain::Arbitrum as u64, Chain::ArbitrumGoerli as u64];