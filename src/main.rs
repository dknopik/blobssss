use alloy::consensus::{SidecarBuilder, SimpleCoder};
use alloy::hex::FromHex;
use alloy::network::{Ethereum, TransactionBuilder};
use alloy::network::{EthereumWallet, NetworkWallet};
use alloy::primitives::{Address, B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use clap::Parser;
use eyre::{eyre, Result, WrapErr};
use futures::future::join_all;
use rand::prelude::*;
use std::time::Duration;
use tokio::time::{interval, MissedTickBehavior};
use url::Url;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long, value_delimiter = ',', num_args = 1..)]
    rpcs: Vec<Url>,
    #[arg(short, long)]
    key: String,
    #[arg(long, default_value_t = 3)]
    min: u8,
    #[arg(long, default_value_t = 3)]
    max: u8,
}
const GWEI: u128 = 1_000_000_000;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.max < args.min || args.max == 0 {
        return Err(eyre!("inconsistent min & max"));
    }

    let providers: Vec<_> = args
        .rpcs
        .into_iter()
        .map(|url| ProviderBuilder::new().on_http(url))
        .collect();

    let parent_wallet = EthereumWallet::from(
        PrivateKeySigner::from_bytes(
            &B256::from_hex(&args.key).wrap_err("while hex decoding private key")?,
        )
        .wrap_err("while parsing private key")?,
    );

    let child_wallets: Vec<_> = (0..args.max)
        .map(|_| EthereumWallet::from(PrivateKeySigner::random()))
        .collect();

    let provider = providers.first().unwrap();
    let balance = provider.get_balance(addr_of(&parent_wallet)).await?;
    let mut nonce = provider
        .get_transaction_count(addr_of(&parent_wallet))
        .await?;
    let chain_id = provider.get_chain_id().await?;

    println!(
        "running on chain {chain_id} with addr {} on nonce {nonce} and {balance}wei",
        addr_of(&parent_wallet)
    );

    let distribute_each = balance / U256::from(args.max + 1);

    let sidecar: SidecarBuilder<SimpleCoder> = SidecarBuilder::from_slice(b"spam");
    let sidecar = sidecar.build()?;

    let mut waiting = vec![];
    for wallet in &child_wallets {
        println!("funding {distribute_each}wei to {}", addr_of(wallet));
        let tx = TransactionRequest::default()
            .with_to(addr_of(wallet))
            .with_nonce(nonce)
            .with_max_fee_per_gas(10 * GWEI)
            .with_max_priority_fee_per_gas(GWEI)
            .with_value(distribute_each)
            .with_chain_id(chain_id)
            .with_from(addr_of(&parent_wallet))
            .with_gas_limit(21_000)
            .build(&parent_wallet)
            .await?;
        waiting.push(provider.send_tx_envelope(tx).await?.register());
        nonce += 1;
    }
    join_all(waiting)
        .await
        .into_iter()
        .try_for_each(|e| e.map(|_| ()))?;
    println!("done funding");

    let mut interval = interval(Duration::from_secs(12));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let num = thread_rng().gen_range(args.min..=args.max);
        println!("sending {num} tx");
        for idx in 0..num {
            let provider = providers.iter().choose(&mut thread_rng()).unwrap();
            let wallet = child_wallets.get(idx as usize).unwrap();
            let nonce = match provider.get_transaction_count(addr_of(wallet)).await {
                Ok(nonce) => nonce,
                Err(err) => {
                    eprintln!("Error getting nonce: {err}");
                    continue;
                }
            };
            let tx = TransactionRequest::default()
                .with_to(addr_of(wallet))
                .with_nonce(nonce)
                .with_max_fee_per_gas(10 * GWEI)
                .with_max_fee_per_blob_gas(10 * GWEI)
                .with_max_priority_fee_per_gas(GWEI)
                .with_chain_id(chain_id)
                .with_value(U256::ZERO)
                .with_from(addr_of(wallet))
                .with_gas_limit(21_000)
                .with_blob_sidecar(sidecar.clone())
                .build(&wallet)
                .await?;
            if let Err(err) = provider.send_tx_envelope(tx).await {
                eprintln!("Error sending tx: {err}");
            }
        }
    }
}

fn addr_of(wallet: &EthereumWallet) -> Address {
    <EthereumWallet as NetworkWallet<Ethereum>>::default_signer_address(wallet)
}
