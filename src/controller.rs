use crate::event_handler::EventHandler;
use crate::net_utils;
use crate::net_utils::do_connect_peer;
use crate::wallet::Wallet;
use anyhow::{bail, Result};
use bitcoin::blockdata::constants::genesis_block;
use bitcoind::Client;
use database::ldk_database::LdkDatabase;
use database::wallet_database::WalletDatabase;
use lightning::chain::keysinterface::{InMemorySigner, KeysInterface, KeysManager, Recipient};
use lightning::chain::{self, ChannelMonitorUpdateStatus};
use lightning::chain::{chainmonitor, Watch};
use lightning::chain::{BestBlock, Filter};
use lightning::ln::channelmanager;
use lightning::ln::channelmanager::{
    ChainParameters, ChannelManagerReadArgs, SimpleArcChannelManager,
};
use lightning::ln::peer_handler::{IgnoringMessageHandler, MessageHandler, SimpleArcPeerManager};
use lightning::onion_message::SimpleArcOnionMessenger;
use lightning::routing::gossip::{self, P2PGossipSync};
use lightning::routing::scoring::{ProbabilisticScorer, ProbabilisticScoringParameters};
use lightning::util::config::UserConfig;
use lightning_background_processor::{BackgroundProcessor, GossipSync};
use lightning_block_sync::init;
use lightning_block_sync::poll;
use lightning_block_sync::SpvClient;
use lightning_block_sync::UnboundedCache;
use lightning_invoice::payment;
use lightning_invoice::utils::DefaultRouter;
use lightning_net_tokio::SocketDescriptor;
use log::{error, info};
use logger::KndLogger;
use rand::{thread_rng, Rng};
use settings::Settings;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

pub(crate) struct Controller {
    peer_manager: Arc<PeerManager>,
    network_graph: Arc<NetworkGraph>,
    wallet: Arc<Wallet>,
}

pub(crate) trait LightningMetrics {
    fn num_nodes(&self) -> usize;

    fn num_channels(&self) -> usize;

    fn num_peers(&self) -> usize;

    fn wallet_balance(&self) -> u64;
}

impl LightningMetrics for Controller {
    fn num_nodes(&self) -> usize {
        self.network_graph.read_only().nodes().len()
    }

    fn num_channels(&self) -> usize {
        self.network_graph.read_only().channels().len()
    }

    fn num_peers(&self) -> usize {
        self.peer_manager.get_peer_node_ids().len()
    }

    fn wallet_balance(&self) -> u64 {
        match self.wallet.balance() {
            Ok(balance) => balance,
            Err(e) => {
                error!("Unable to get wallet balance for metrics: {}", e);
                0
            }
        }
    }
}

impl Controller {
    pub fn stop(&self) {
        // Disconnect our peers and stop accepting new connections. This ensures we don't continue
        // updating our channel data after we've stopped the background processor.
        self.peer_manager.disconnect_all_peers();
    }

    pub async fn start_ldk(
        settings: &Settings,
        database: Arc<LdkDatabase>,
        shutdown_flag: Arc<AtomicBool>,
    ) -> Result<(Controller, BackgroundProcessor)> {
        // Initialize our bitcoind client.
        let bitcoind_client = match Client::new(settings, tokio::runtime::Handle::current()).await {
            Ok(client) => {
                info!(
                    "Connected to bitcoind at {}:{}",
                    settings.bitcoind_rpc_host, settings.bitcoind_rpc_port
                );
                Arc::new(client)
            }
            Err(e) => {
                bail!("Failed to connect to bitcoind client: {}", e);
            }
        };

        // Check that the bitcoind we've connected to is running the network we expect
        let bitcoind_chain = bitcoind_client.get_blockchain_info().await.chain;
        if bitcoind_chain
            != match settings.bitcoin_network {
                bitcoin::Network::Bitcoin => "main",
                bitcoin::Network::Testnet => "test",
                bitcoin::Network::Regtest => "regtest",
                bitcoin::Network::Signet => "signet",
            }
        {
            bail!(
                "Chain argument ({}) didn't match bitcoind chain ({})",
                settings.bitcoin_network,
                bitcoind_chain
            );
        }

        // Initialize the FeeEstimator
        // BitcoindClient implements the FeeEstimator trait, so it'll act as our fee estimator.
        let fee_estimator = bitcoind_client.clone();

        // Initialize the BroadcasterInterface
        // BitcoindClient implements the BroadcasterInterface trait, so it'll act as our transaction
        // broadcaster.
        let broadcaster = bitcoind_client.clone();

        // Initialize the ChainMonitor
        let chain_monitor: Arc<ChainMonitor> = Arc::new(chainmonitor::ChainMonitor::new(
            None,
            broadcaster.clone(),
            KndLogger::global(),
            fee_estimator.clone(),
            database.clone(),
        ));

        let is_first_start = database.is_first_start().await?;
        // Initialize the KeysManager
        // The key seed that we use to derive the node privkey (that corresponds to the node pubkey) and
        // other secret key material.
        let seed = if !is_first_start {
            info!("Fetching keys from database");
            let (_, private) = database.fetch_keys().await?;
            private
        } else {
            info!("Initializing node. Generating keys.");
            let seed: [u8; 32] = thread_rng().gen();
            seed
        };
        let cur = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        let keys_manager = Arc::new(KeysManager::new(&seed, cur.as_secs(), cur.subsec_nanos()));

        // Initialize bitcoin wallet.
        let wallet_database = WalletDatabase::new(settings).await?;
        let wallet = Arc::new(Wallet::new(
            &seed,
            settings,
            bitcoind_client.clone(),
            wallet_database,
        )?);

        // Initialize the ChannelManager
        let mut channelmonitors = database
            .fetch_channel_monitors(keys_manager.clone())
            .await?;
        let mut user_config = UserConfig::default();
        user_config
            .channel_handshake_limits
            .force_announced_channel_preference = false;
        let (channel_manager_blockhash, channel_manager) = {
            if is_first_start {
                let getinfo_resp = bitcoind_client.get_blockchain_info().await;

                let chain_params = ChainParameters {
                    network: settings.bitcoin_network,
                    best_block: BestBlock::new(
                        getinfo_resp.latest_blockhash,
                        getinfo_resp.latest_height as u32,
                    ),
                };
                let new_channel_manager = channelmanager::ChannelManager::new(
                    fee_estimator.clone(),
                    chain_monitor.clone(),
                    broadcaster.clone(),
                    KndLogger::global(),
                    keys_manager.clone(),
                    user_config,
                    chain_params,
                );
                (getinfo_resp.latest_blockhash, new_channel_manager)
            } else {
                let mut channel_monitor_mut_references = Vec::new();
                for (_, channel_monitor) in channelmonitors.iter_mut() {
                    channel_monitor_mut_references.push(channel_monitor);
                }
                let read_args = ChannelManagerReadArgs::new(
                    keys_manager.clone(),
                    fee_estimator.clone(),
                    chain_monitor.clone(),
                    broadcaster.clone(),
                    KndLogger::global(),
                    user_config,
                    channel_monitor_mut_references,
                );
                database.fetch_channel_manager(read_args).await?
            }
        };

        if is_first_start {
            database
                .persist_keys(&channel_manager.get_our_node_id(), &seed)
                .await?;
        }

        // Sync ChannelMonitors and ChannelManager to chain tip
        let mut chain_listener_channel_monitors = Vec::new();
        let mut cache = UnboundedCache::new();
        let mut chain_tip: Option<poll::ValidatedBlockHeader> = None;
        if !is_first_start {
            let mut chain_listeners = vec![(
                channel_manager_blockhash,
                &channel_manager as &(dyn chain::Listen + Send + Sync),
            )];

            for (blockhash, channel_monitor) in channelmonitors.drain(..) {
                let outpoint = channel_monitor.get_funding_txo().0;
                chain_listener_channel_monitors.push((
                    blockhash,
                    (
                        channel_monitor,
                        broadcaster.clone(),
                        fee_estimator.clone(),
                        KndLogger::global(),
                    ),
                    outpoint,
                ));
            }

            for monitor_listener_info in chain_listener_channel_monitors.iter_mut() {
                chain_listeners.push((
                    monitor_listener_info.0,
                    &monitor_listener_info.1 as &(dyn chain::Listen + Send + Sync),
                ));
            }
            chain_tip = Some(
                init::synchronize_listeners(
                    &mut bitcoind_client.deref(),
                    settings.bitcoin_network,
                    &mut cache,
                    chain_listeners,
                )
                .await
                .unwrap(),
            );
        }

        // Give ChannelMonitors to ChainMonitor
        for item in chain_listener_channel_monitors.drain(..) {
            let channel_monitor = item.1 .0;
            let funding_outpoint = item.2;
            assert_eq!(
                chain_monitor.watch_channel(funding_outpoint, channel_monitor),
                ChannelMonitorUpdateStatus::Completed
            );
        }

        // Initialize the P2PGossipSync
        let genesis = genesis_block(settings.bitcoin_network).header.block_hash();
        let network_graph = Arc::new(
            database
                .fetch_graph()
                .await?
                .unwrap_or_else(|| NetworkGraph::new(genesis, KndLogger::global())),
        );

        let gossip_sync = Arc::new(P2PGossipSync::new(
            network_graph.clone(),
            None::<Arc<dyn chain::Access + Send + Sync>>,
            KndLogger::global(),
        ));

        // Initialize the PeerManager
        let channel_manager: Arc<ChannelManager> = Arc::new(channel_manager);
        let onion_messenger: Arc<OnionMessenger> = Arc::new(OnionMessenger::new(
            keys_manager.clone(),
            KndLogger::global(),
            IgnoringMessageHandler {},
        ));
        let ephemeral_bytes: [u8; 32] = thread_rng().gen();
        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let lightning_msg_handler = MessageHandler {
            chan_handler: channel_manager.clone(),
            route_handler: gossip_sync.clone(),
            onion_message_handler: onion_messenger.clone(),
        };
        let peer_manager: Arc<PeerManager> = Arc::new(PeerManager::new(
            lightning_msg_handler,
            keys_manager.get_node_secret(Recipient::Node).unwrap(),
            current_time.try_into().unwrap(),
            &ephemeral_bytes,
            KndLogger::global(),
            IgnoringMessageHandler {},
        ));

        // ## Running LDK
        // Initialize networking

        let peer_manager_connection_handler = peer_manager.clone();
        let listening_port = settings.knd_peer_port;
        let stop_listen = shutdown_flag.clone();
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", listening_port))
                .await
                .expect(
                    "Failed to bind to listen port - is something else already listening on it?",
                );
            loop {
                let peer_mgr = peer_manager_connection_handler.clone();
                let tcp_stream = listener.accept().await.unwrap().0;
                if stop_listen.load(Ordering::Acquire) {
                    return;
                }
                tokio::spawn(async move {
                    lightning_net_tokio::setup_inbound(
                        peer_mgr.clone(),
                        tcp_stream.into_std().unwrap(),
                    )
                    .await;
                });
            }
        });

        // Connect and Disconnect Blocks
        if chain_tip.is_none() {
            chain_tip = Some(
                init::validate_best_block_header(&mut bitcoind_client.deref())
                    .await
                    .unwrap(),
            );
        }
        let channel_manager_listener = channel_manager.clone();
        let chain_monitor_listener = chain_monitor.clone();
        let bitcoind_block_source = bitcoind_client.clone();
        let network = settings.bitcoin_network;
        tokio::spawn(async move {
            let mut derefed = bitcoind_block_source.deref();
            let chain_poller = poll::ChainPoller::new(&mut derefed, network);
            let chain_listener = (chain_monitor_listener, channel_manager_listener);
            let mut spv_client = SpvClient::new(
                chain_tip.unwrap(),
                chain_poller,
                &mut cache,
                &chain_listener,
            );
            loop {
                spv_client.poll_best_tip().await.unwrap();
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });

        // Handle LDK Events
        let event_handler = EventHandler::new(
            channel_manager.clone(),
            bitcoind_client,
            keys_manager.clone(),
            settings.bitcoin_network,
            network_graph.clone(),
            wallet.clone(),
            database.clone()
        );

        // Initialize routing ProbabilisticScorer
        let scorer = Arc::new(Mutex::new(
            database
                .fetch_scorer(
                    ProbabilisticScoringParameters::default(),
                    network_graph.clone(),
                )
                .await?
                .unwrap_or_else(|| {
                    ProbabilisticScorer::new(
                        ProbabilisticScoringParameters::default(),
                        network_graph.clone(),
                        KndLogger::global(),
                    )
                }),
        ));

        // Create InvoicePayer
        let router = DefaultRouter::new(
            network_graph.clone(),
            KndLogger::global(),
            keys_manager.get_secure_random_bytes(),
            scorer.clone(),
        );
        let invoice_payer = Arc::new(InvoicePayer::new(
            channel_manager.clone(),
            router,
            KndLogger::global(),
            event_handler,
            payment::Retry::Timeout(Duration::from_secs(10)),
        ));

        // Background Processing
        let background_processor = BackgroundProcessor::start(
            database.clone(),
            invoice_payer.clone(),
            chain_monitor.clone(),
            channel_manager.clone(),
            GossipSync::p2p(gossip_sync.clone()),
            peer_manager.clone(),
            KndLogger::global(),
            Some(scorer),
        );

        // Regularly reconnect to channel peers.
        let connect_cm = channel_manager.clone();
        let connect_pm = peer_manager.clone();
        let stop_connect = shutdown_flag.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;

                match database.fetch_peers().await {
                    Ok(peers) => {
                        let node_ids = connect_pm.get_peer_node_ids();
                        for node_id in connect_cm
                            .list_channels()
                            .iter()
                            .map(|chan| chan.counterparty.node_id)
                            .filter(|id| !node_ids.contains(id))
                        {
                            if stop_connect.load(Ordering::Acquire) {
                                return;
                            }
                            for peer in peers.iter() {
                                if peer.public_key == node_id {
                                    let _ = do_connect_peer(
                                        peer.public_key,
                                        peer.socket_addr,
                                        connect_pm.clone(),
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to fetch peers: {}", e);
                    }
                }
            }
        });

        // Regularly broadcast our node_announcement. This is only required (or possible) if we have
        // some public channels, and is only useful if we have public listen address(es) to announce.
        // In a production environment, this should occur only after the announcement of new channels
        // to avoid churn in the global network graph.
        if settings.knd_node_name.len() > 32 {
            bail!("Node Alias can not be longer than 32 bytes");
        }
        let mut alias = [0; 32];
        alias[..settings.knd_node_name.len()].copy_from_slice(settings.knd_node_name.as_bytes());
        let peer_man = Arc::clone(&peer_manager);
        if !settings.knd_listen_addresses.is_empty() {
            let addresses = settings.knd_listen_addresses.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    peer_man.broadcast_node_announcement(
                        [0; 3],
                        alias,
                        addresses.iter().map(|s| net_utils::to_address(s)).collect(),
                    );
                }
            });
        }

        Ok((
            Controller {
                peer_manager,
                network_graph,
                wallet,
            },
            background_processor,
        ))
    }
}

type ChainMonitor = chainmonitor::ChainMonitor<
    InMemorySigner,
    Arc<dyn Filter + Send + Sync>,
    Arc<Client>,
    Arc<Client>,
    Arc<KndLogger>,
    Arc<LdkDatabase>,
>;

pub(crate) type PeerManager = SimpleArcPeerManager<
    SocketDescriptor,
    ChainMonitor,
    Client,
    Client,
    dyn chain::Access + Send + Sync,
    KndLogger,
>;

pub(crate) type ChannelManager = SimpleArcChannelManager<ChainMonitor, Client, Client, KndLogger>;

pub(crate) type InvoicePayer<E> =
    payment::InvoicePayer<Arc<ChannelManager>, Router, Arc<KndLogger>, E>;

type Router = DefaultRouter<
    Arc<NetworkGraph>,
    Arc<KndLogger>,
    Arc<Mutex<ProbabilisticScorer<Arc<NetworkGraph>, Arc<KndLogger>>>>,
>;

pub(crate) type NetworkGraph = gossip::NetworkGraph<Arc<KndLogger>>;

type OnionMessenger = SimpleArcOnionMessenger<KndLogger>;
