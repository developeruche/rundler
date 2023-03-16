use crate::common::contracts::get_code_hashes::{CodeHashesResult, GETCODEHASHES_BYTECODE};
use anyhow::Context;
use ethers::abi::{AbiDecode, AbiEncode, RawLog};
use ethers::contract::builders::ContractCall;
use ethers::contract::{Contract, ContractDeployer, ContractError};
use ethers::core::k256::ecdsa::SigningKey;
use ethers::middleware::SignerMiddleware;
use ethers::providers::{
    Http, HttpClientError, JsonRpcClient, Middleware, PendingTransaction, Provider, ProviderError,
};
use ethers::signers::{LocalWallet, Signer};
use ethers::types::{
    Address, BlockId, Bytes, Eip1559TransactionRequest, Log, TransactionReceipt,
    TransactionRequest, H256,
};
use ethers::utils;
use serde_json::Value;
use std::future::Future;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;
use std::{error, mem};

/// Creates a provider that connects to a locally running Geth node on its
/// default port of 8545.
pub fn new_local_provider() -> Arc<Provider<Http>> {
    // Set low interval because Geth node in --dev mode mines very quickly once
    // it sees a transaction. By default, the provider waits seven seconds to
    // poll for new blocks, which is excruciating while testing.
    let provider = Provider::<Http>::try_from("http://localhost:8545")
        .expect("localhost url should parse")
        .interval(Duration::from_millis(100));
    Arc::new(provider)
}

/// Given a provider connected to a Geth node in --dev mode, grants a large
/// amount of ETH to the specified address.
pub async fn grant_dev_eth(provider: &Provider<Http>, to: Address) -> anyhow::Result<()> {
    // A Geth node in --dev mode has one account with massive amounts of ETH.
    let funder_address = *provider
        .get_accounts()
        .await
        .context("should be able to get accounts from node")?
        .first()
        .context("a Geth node in dev mode should have one account")?;
    // 1000 ETH ought to be enough for anyone.
    let value = utils::parse_ether(1000).unwrap();
    let tx = provider.send_transaction(
        TransactionRequest::pay(to, value).from(funder_address),
        None,
    );
    await_mined_tx(tx, "grant ETH").await?;
    Ok(())
}

/// Waits for a pending transaction to be mined, providing appropriate error
/// messages for each point of failure.
pub async fn await_mined_tx<'a, Fut, C, Err>(
    tx: Fut,
    action: &str,
) -> anyhow::Result<TransactionReceipt>
where
    Fut: Future<Output = Result<PendingTransaction<'a, C>, Err>>,
    C: JsonRpcClient + 'a,
    Err: error::Error + Send + Sync + 'static,
{
    tx.await
        .with_context(|| format!("should send transaction to {action}"))?
        .await
        .with_context(|| format!("should wait for transaction to {action}"))?
        .with_context(|| format!("transaction to {action} should not be dropped"))
}

/// Waits for a contract deployment, providing appropriate error messages.
pub async fn await_contract_deployment<M, C>(
    deployer: Result<ContractDeployer<M, C>, ContractError<M>>,
    contract_name: &str,
) -> anyhow::Result<C>
where
    M: Middleware + 'static,
    C: From<Contract<M>>,
{
    deployer
        .with_context(|| format!("should create deployer for {contract_name}"))?
        .send()
        .await
        .with_context(|| format!("should deploy {contract_name}"))
}

/// Changes out a contract object's signer and returns a new contract of the
/// same type. Needed because although the general-purpose `Contract` has a
/// `.connect()` method to do this, specialized contract objects do not.
pub fn connect_contract<M, C>(contract: &C, provider: Arc<M>) -> C
where
    M: Clone + Middleware,
    C: Deref<Target = Contract<M>> + From<Contract<M>>,
{
    contract.connect(provider).into()
}

/// Packs an address followed by call data into a single `Bytes`. This is used
/// in ERC-4337 for calling wallets, factories, and paymasters.
pub fn compact_call_data<M, D>(address: Address, call: ContractCall<M, D>) -> Bytes {
    let mut bytes = address.as_bytes().to_vec();
    if let Some(call_data) = call.tx.data() {
        bytes.extend(call_data);
    }
    bytes.into()
}

/// Converts an ethers `Log` into an ethabi `RawLog`.
pub fn log_to_raw_log(log: Log) -> RawLog {
    let Log { topics, data, .. } = log;
    RawLog {
        topics,
        data: data.to_vec(),
    }
}

pub async fn get_chain_id(provider: &Provider<Http>) -> anyhow::Result<u32> {
    Ok(provider
        .get_chainid()
        .await
        .context("should get chain id")?
        .as_u32())
}

/// Converts a block id, which may be something like "latest" which can refer to
/// different blocks over time, into one which references a fixed block by its
/// hash.
pub async fn get_static_block_id(
    provider: &Provider<Http>,
    block_id: BlockId,
) -> anyhow::Result<BlockId> {
    Ok(get_block_hash(provider, block_id).await?.into())
}

async fn get_block_hash(provider: &Provider<Http>, block_id: BlockId) -> anyhow::Result<H256> {
    if let BlockId::Hash(hash) = block_id {
        return Ok(hash);
    }
    provider
        .get_block(block_id)
        .await
        .context("should load block to get hash")?
        .context("block should exist to get latest hash")?
        .hash
        .context("hash should be present on block")
}

/// Creates a client that can send transactions and sign them with a secret
/// based on a fixed id. Can be used to generate accounts with deterministic
/// addresses for testing.
pub fn new_test_client(
    provider: Arc<Provider<Http>>,
    test_account_id: u8,
    chain_id: u32,
) -> Arc<SignerMiddleware<Arc<Provider<Http>>, LocalWallet>> {
    let wallet = new_test_wallet(test_account_id, chain_id);
    Arc::new(SignerMiddleware::new(provider, wallet))
}

/// Creates a wallet whose secret is based on a fixed id. Differs from
/// `new_test_client` in that a wallet on its own can only sign messages but
/// not send transactions.
pub fn new_test_wallet(test_account_id: u8, chain_id: u32) -> LocalWallet {
    let bytes = test_signing_key_bytes(test_account_id);
    let key = SigningKey::from_bytes(&bytes).expect("should create signing key for test wallet");
    LocalWallet::from(key).with_chain_id(chain_id)
}

pub fn test_signing_key_bytes(test_account_id: u8) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    bytes[31] = test_account_id;
    bytes
}

/// Hashes together the code from all the provided addresses. The order of the input addresses does
/// not matter.
pub async fn get_code_hash(
    provider: &Provider<Http>,
    mut addresses: Vec<Address>,
    block_id: Option<BlockId>,
) -> Result<H256, anyhow::Error> {
    addresses.sort();
    let out: CodeHashesResult =
        call_constructor(provider, &GETCODEHASHES_BYTECODE, addresses, block_id)
            .await
            .context("should compute code hashes")?;
    Ok(H256(out.hash))
}

async fn call_constructor<Args: AbiEncode, Ret: AbiDecode>(
    provider: &Provider<Http>,
    bytecode: &Bytes,
    args: Args,
    block_id: Option<BlockId>,
) -> anyhow::Result<Ret> {
    let mut data = bytecode.to_vec();
    data.extend(AbiEncode::encode(args));
    let tx = Eip1559TransactionRequest {
        data: Some(data.into()),
        ..Default::default()
    };
    let error = provider
        .call(&tx.into(), block_id)
        .await
        .err()
        .context("called constructor should revert")?;
    let revert_data = get_revert_data(error).context("should call constructor")?;
    Ret::decode_hex(revert_data).context("should decode revert data from called constructor")
}

/// Extracts the revert reason as a hex string if this is a revert error,
/// otherwise returns the original error.
pub fn get_revert_data(mut error: ProviderError) -> Result<String, ProviderError> {
    let ProviderError::JsonRpcClientError(dyn_error) = &mut error else {
        return Err(error);
    };
    let Some(HttpClientError::JsonRpcError(jsonrpc_error)) = dyn_error.downcast_mut::<HttpClientError>() else {
        return Err(error)
    };
    match &mut jsonrpc_error.data {
        Some(Value::String(s)) => Ok(mem::take(s)),
        _ => Err(error),
    }
}