use crate::convert::{BlockchainInfo, FeeResponse, RawTx};
use base64;
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::consensus::encode;
use bitcoin::hash_types::{BlockHash, Txid};
use lightning::chain::chaininterface::{BroadcasterInterface, ConfirmationTarget, FeeEstimator};
use lightning_block_sync::http::HttpEndpoint;
use lightning_block_sync::rpc::RpcClient;
use lightning_block_sync::{AsyncBlockSourceResult, BlockData, BlockHeaderData, BlockSource};
use log::info;
use serde_json;
use settings::Settings;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Client {
    bitcoind_rpc_client: Arc<RpcClient>,
    fees: Arc<HashMap<Target, AtomicU32>>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub enum Target {
    Background,
    Normal,
    HighPriority,
}

impl BlockSource for &Client {
    fn get_header<'a>(
        &'a self,
        header_hash: &'a BlockHash,
        height_hint: Option<u32>,
    ) -> AsyncBlockSourceResult<'a, BlockHeaderData> {
        Box::pin(async move {
            self.bitcoind_rpc_client
                .get_header(header_hash, height_hint)
                .await
        })
    }

    fn get_block<'a>(
        &'a self,
        header_hash: &'a BlockHash,
    ) -> AsyncBlockSourceResult<'a, BlockData> {
        Box::pin(async move { self.bitcoind_rpc_client.get_block(header_hash).await })
    }

    fn get_best_block(&self) -> AsyncBlockSourceResult<(BlockHash, Option<u32>)> {
        Box::pin(async move { self.bitcoind_rpc_client.get_best_block().await })
    }
}

/// The minimum feerate we are allowed to send, as specify by LDK.
const MIN_FEERATE: u32 = 253;

impl Client {
    pub async fn new(settings: &Settings) -> std::io::Result<Self> {
        let bitcoind_rpc_client = Client::get_new_rpc_client(settings)?;
        bitcoind_rpc_client
            .call_method::<BlockchainInfo>("getblockchaininfo", &[])
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::PermissionDenied,
				"Failed to make initial call to bitcoind - please check your RPC user/password and access settings")
            })?;
        let mut fees: HashMap<Target, AtomicU32> = HashMap::new();
        fees.insert(Target::Background, AtomicU32::new(MIN_FEERATE));
        fees.insert(Target::Normal, AtomicU32::new(2000));
        fees.insert(Target::HighPriority, AtomicU32::new(5000));
        let client = Self {
            bitcoind_rpc_client: Arc::new(bitcoind_rpc_client),
            fees: Arc::new(fees),
        };
        Client::poll_for_fee_estimates(client.fees.clone(), client.bitcoind_rpc_client.clone());
        info!(
            "Connected to bitcoind at {}:{}",
            settings.bitcoind_rpc_host, settings.bitcoind_rpc_port
        );
        Ok(client)
    }

    fn get_new_rpc_client(settings: &Settings) -> std::io::Result<RpcClient> {
        let mut file = File::open(settings.bitcoin_cookie_path.clone())?;
        let mut cookie = String::new();
        file.read_to_string(&mut cookie)?;
        let credentials = base64::encode(cookie.as_bytes());
        let http_endpoint = HttpEndpoint::for_host(settings.bitcoind_rpc_host.clone())
            .with_port(settings.bitcoind_rpc_port);
        RpcClient::new(&credentials, http_endpoint)
    }

    fn poll_for_fee_estimates(fees: Arc<HashMap<Target, AtomicU32>>, rpc_client: Arc<RpcClient>) {
        tokio::spawn(async move {
            loop {
                let background_estimate = {
                    let background_conf_target = serde_json::json!(144);
                    let background_estimate_mode = serde_json::json!("ECONOMICAL");
                    let resp = rpc_client
                        .call_method::<FeeResponse>(
                            "estimatesmartfee",
                            &[background_conf_target, background_estimate_mode],
                        )
                        .await
                        .unwrap();
                    match resp.feerate_sat_per_kw {
                        Some(feerate) => std::cmp::max(feerate, MIN_FEERATE),
                        None => MIN_FEERATE,
                    }
                };

                let normal_estimate = {
                    let normal_conf_target = serde_json::json!(18);
                    let normal_estimate_mode = serde_json::json!("ECONOMICAL");
                    let resp = rpc_client
                        .call_method::<FeeResponse>(
                            "estimatesmartfee",
                            &[normal_conf_target, normal_estimate_mode],
                        )
                        .await
                        .unwrap();
                    match resp.feerate_sat_per_kw {
                        Some(feerate) => std::cmp::max(feerate, MIN_FEERATE),
                        None => 2000,
                    }
                };

                let high_prio_estimate = {
                    let high_prio_conf_target = serde_json::json!(6);
                    let high_prio_estimate_mode = serde_json::json!("CONSERVATIVE");
                    let resp = rpc_client
                        .call_method::<FeeResponse>(
                            "estimatesmartfee",
                            &[high_prio_conf_target, high_prio_estimate_mode],
                        )
                        .await
                        .unwrap();

                    match resp.feerate_sat_per_kw {
                        Some(feerate) => std::cmp::max(feerate, MIN_FEERATE),
                        None => 5000,
                    }
                };

                fees.get(&Target::Background)
                    .unwrap()
                    .store(background_estimate, Ordering::Release);
                fees.get(&Target::Normal)
                    .unwrap()
                    .store(normal_estimate, Ordering::Release);
                fees.get(&Target::HighPriority)
                    .unwrap()
                    .store(high_prio_estimate, Ordering::Release);
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    pub async fn send_raw_transaction(&self, raw_tx: RawTx) {
        let raw_tx_json = serde_json::json!(raw_tx.0);
        self.bitcoind_rpc_client
            .call_method::<Txid>("sendrawtransaction", &[raw_tx_json])
            .await
            .unwrap();
    }

    pub async fn get_blockchain_info(&self) -> BlockchainInfo {
        self.bitcoind_rpc_client
            .call_method::<BlockchainInfo>("getblockchaininfo", &[])
            .await
            .unwrap()
    }
}

impl FeeEstimator for Client {
    fn get_est_sat_per_1000_weight(&self, confirmation_target: ConfirmationTarget) -> u32 {
        match confirmation_target {
            ConfirmationTarget::Background => self
                .fees
                .get(&Target::Background)
                .unwrap()
                .load(Ordering::Acquire),
            ConfirmationTarget::Normal => self
                .fees
                .get(&Target::Normal)
                .unwrap()
                .load(Ordering::Acquire),
            ConfirmationTarget::HighPriority => self
                .fees
                .get(&Target::HighPriority)
                .unwrap()
                .load(Ordering::Acquire),
        }
    }
}

impl BroadcasterInterface for Client {
    fn broadcast_transaction(&self, tx: &Transaction) {
        let bitcoind_rpc_client = self.bitcoind_rpc_client.clone();
        let tx_serialized = serde_json::json!(encode::serialize_hex(tx));
        tokio::spawn(async move {
            // This may error due to RL calling `broadcast_transaction` with the same transaction
            // multiple times, but the error is safe to ignore.
            match bitcoind_rpc_client
                .call_method::<Txid>("sendrawtransaction", &[tx_serialized])
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    let err_str = e.get_ref().unwrap().to_string();
                    if !err_str.contains("Transaction already in block chain")
                        && !err_str.contains("Inputs missing or spent")
                        && !err_str.contains("bad-txns-inputs-missingorspent")
                        && !err_str.contains("txn-mempool-conflict")
                        && !err_str.contains("non-BIP68-final")
                        && !err_str.contains("insufficient fee, rejecting replacement ")
                    {
                        panic!("{}", e);
                    }
                }
            }
        });
    }
}
