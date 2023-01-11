use anyhow::Result;
use async_trait::async_trait;
use bitcoin::{consensus::deserialize, hashes::Hash, secp256k1::PublicKey, Network, Txid};
use hex::FromHex;
use lightning::{
    chain::transaction::OutPoint,
    ln::{
        channelmanager::{ChannelCounterparty, ChannelDetails},
        features::{InitFeatures, NodeFeatures},
    },
    routing::gossip::{NodeAlias, NodeAnnouncementInfo, NodeInfo},
    util::config::UserConfig,
};
use lightning_knd::api::{LightningInterface, OpenChannelResult};
use test_utils::random_public_key;

pub struct MockLightning {
    pub num_peers: usize,
    pub num_nodes: usize,
    pub num_channels: usize,
    pub wallet_balance: u64,
    pub channels: Vec<ChannelDetails>,
}

impl Default for MockLightning {
    fn default() -> Self {
        let channel = ChannelDetails {
            channel_id: [1u8; 32],
            counterparty: ChannelCounterparty {
                node_id: PublicKey::from_slice(&[
                    2, 2, 117, 91, 71, 83, 52, 189, 154, 86, 163, 23, 253, 35, 223, 226, 100, 177,
                    147, 188, 189, 115, 34, 250, 163, 233, 116, 3, 23, 4, 6, 130, 102,
                ])
                .unwrap(),
                features: InitFeatures::empty(),
                unspendable_punishment_reserve: 5000,
                forwarding_info: None,
                outbound_htlc_minimum_msat: Some(1000),
                outbound_htlc_maximum_msat: Some(100),
            },
            funding_txo: Some(OutPoint {
                txid: Txid::all_zeros(),
                index: 2,
            }),
            channel_type: None,
            short_channel_id: Some(34234124),
            outbound_scid_alias: None,
            inbound_scid_alias: None,
            channel_value_satoshis: 1000000,
            unspendable_punishment_reserve: Some(10000),
            user_channel_id: 3434232,
            balance_msat: 10001,
            outbound_capacity_msat: 100000,
            next_outbound_htlc_limit_msat: 500,
            inbound_capacity_msat: 200000,
            confirmations_required: Some(3),
            confirmations: Some(10),
            force_close_spend_delay: Some(6),
            is_outbound: true,
            is_channel_ready: true,
            is_usable: true,
            is_public: true,
            inbound_htlc_minimum_msat: Some(300),
            inbound_htlc_maximum_msat: Some(300000),
            config: None,
        };
        Self {
            num_peers: 5,
            num_nodes: 6,
            num_channels: 7,
            wallet_balance: 8,
            channels: vec![channel],
        }
    }
}

#[async_trait]
impl LightningInterface for MockLightning {
    fn alias(&self) -> String {
        "test".to_string()
    }
    fn identity_pubkey(&self) -> PublicKey {
        random_public_key()
    }

    fn graph_num_nodes(&self) -> usize {
        self.num_nodes
    }

    fn graph_num_channels(&self) -> usize {
        self.num_channels
    }

    fn block_height(&self) -> usize {
        50000
    }

    fn network(&self) -> bitcoin::Network {
        Network::Bitcoin
    }
    fn num_active_channels(&self) -> usize {
        0
    }

    fn num_inactive_channels(&self) -> usize {
        0
    }

    fn num_pending_channels(&self) -> usize {
        0
    }
    fn num_peers(&self) -> usize {
        self.num_peers
    }

    fn wallet_balance(&self) -> u64 {
        self.wallet_balance
    }

    fn version(&self) -> String {
        "v0.1".to_string()
    }

    fn list_channels(&self) -> Vec<ChannelDetails> {
        self.channels.clone()
    }

    fn get_node(&self, _node_id: PublicKey) -> Option<NodeInfo> {
        Some(NodeInfo {
            channels: vec![],
            lowest_inbound_channel_fees: None,
            announcement_info: Some(NodeAnnouncementInfo {
                features: NodeFeatures::empty(),
                last_update: 1000,
                rgb: [3, 2, 1],
                alias: NodeAlias(*b"test_node                       "),
                addresses: vec![],
                announcement_message: None,
            }),
        })
    }

    async fn open_channel(
        &self,
        _their_network_key: PublicKey,
        _channel_value_satoshis: u64,
        _push_msat: u64,
        _override_config: Option<UserConfig>,
    ) -> Result<OpenChannelResult> {
        let transaction =
            deserialize::<bitcoin::Transaction>(&Vec::<u8>::from_hex(TEST_TX).unwrap()).unwrap();
        let txid = transaction.txid();
        Ok(OpenChannelResult {
            transaction,
            txid,
            channel_id: [1u8; 32],
        })
    }
}

const TEST_TX: &str = "0200000003c26f3eb7932f7acddc5ddd26602b77e7516079b03090a16e2c2f54\
                                    85d1fd600f0100000000ffffffffc26f3eb7932f7acddc5ddd26602b77e75160\
                                    79b03090a16e2c2f5485d1fd600f0000000000ffffffff571fb3e02278217852\
                                    dd5d299947e2b7354a639adc32ec1fa7b82cfb5dec530e0500000000ffffffff\
                                    03e80300000000000002aaeee80300000000000001aa200300000000000001ff\
                                    00000000";
