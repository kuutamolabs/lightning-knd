use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::vec;

use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::hashes::Hash;
use bitcoin::{BlockHash, TxMerkleNode};
use bitcoind::Client;
use database::ldk_database::LdkDatabase;
use database::migrate_database;
use database::payment::{Payment, HTLCStatus, MillisatAmount};
use database::peer::Peer;
use lightning::chain::chainmonitor::ChainMonitor;
use lightning::chain::keysinterface::{InMemorySigner, KeysManager};
use lightning::chain::Filter;
use lightning::ln::{channelmanager, functional_test_utils::*, PaymentPreimage, PaymentSecret, PaymentHash};
use lightning::routing::gossip::NetworkGraph;
use lightning::routing::scoring::{ProbabilisticScorer, ProbabilisticScoringParameters};
use lightning::util::events::{ClosureReason, MessageSendEventsProvider};
use lightning::util::persist::Persister;
use lightning::util::test_utils as ln_utils;
use lightning::{check_added_monitors, check_closed_broadcast, check_closed_event};
use logger::KndLogger;
use test_utils::random_public_key;

use crate::{global_cockroach, teardown};

async fn setup() -> LdkDatabase {
    let (settings, _cockroach) = global_cockroach().await;
    let database = LdkDatabase::new(&settings).await.unwrap();
    database
}

async fn setup_new(database_name: String) -> LdkDatabase {
    let (settings, _cockroach) = global_cockroach().await;
    let mut new_settings = settings.clone();
    new_settings.database_name = database_name;
    migrate_database(&new_settings).await.unwrap();
    let database = LdkDatabase::new(&new_settings).await.unwrap();
    database
}

#[tokio::test(flavor = "multi_thread")]
pub async fn test_key() {
    let database = setup().await;

    assert!(database.is_first_start().await.unwrap());

    let public = random_public_key();
    let private = [1; 32];
    database.persist_keys(&public, &private).await.unwrap();

    let persisted = database.fetch_keys().await.unwrap();
    assert_eq!(public, persisted.0);
    assert_eq!(private, persisted.1);

    assert!(!database.is_first_start().await.unwrap());
    teardown().await;
}

#[tokio::test(flavor = "multi_thread")]
pub async fn test_peers() {
    let database = setup().await;

    let peer = Peer {
        public_key: random_public_key(),
        socket_addr: std::net::SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 1020)),
    };
    database.persist_peer(&peer).await.unwrap();

    let peers = database.fetch_peers().await.unwrap();
    assert!(peers.contains(&peer));

    database.delete_peer(&peer).await;
    let peers = database.fetch_peers().await.unwrap();
    assert!(!peers.contains(&peer));
    teardown().await;
}


/// TODO enable this when cockroach package is updated.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "enums don't work on the old version of cockroach"]
pub async fn test_payments() {
    let database = setup().await;

    let payment = Payment {
        preimage: Some(PaymentPreimage([1; 32])),
        secret: Some(PaymentSecret([2;32])),
        status: HTLCStatus::Succeeded,
        amount_msat: MillisatAmount(50000),
        is_outbound: true
    };
    let payment_hash = PaymentHash([3;32]);
    database.persist_payment(&payment_hash, &payment).await.unwrap();

    let result = database.fetch_payment(&payment_hash).await.unwrap();
    assert_eq!(result, Some(payment));
    teardown().await;
}

// (Test copied from LDK FilesystemPersister).
// Test relaying a few payments and check that the persisted data is updated the appropriate number of times.
#[tokio::test(flavor = "multi_thread")]
pub async fn test_channel_monitors() {
    let database_0 = setup_new("test1".to_string()).await;
    let database_1 = setup_new("test2".to_string()).await;

    // Create the nodes, giving them data databases.
    let chanmon_cfgs = create_chanmon_cfgs(2);
    let mut node_cfgs = create_node_cfgs(2, &chanmon_cfgs);
    let chain_mon_0 = ln_utils::TestChainMonitor::new(
        Some(&chanmon_cfgs[0].chain_source),
        &chanmon_cfgs[0].tx_broadcaster,
        &chanmon_cfgs[0].logger,
        &chanmon_cfgs[0].fee_estimator,
        &database_0,
        &node_cfgs[0].keys_manager,
    );
    let chain_mon_1 = ln_utils::TestChainMonitor::new(
        Some(&chanmon_cfgs[1].chain_source),
        &chanmon_cfgs[1].tx_broadcaster,
        &chanmon_cfgs[1].logger,
        &chanmon_cfgs[1].fee_estimator,
        &database_1,
        &node_cfgs[1].keys_manager,
    );
    node_cfgs[0].chain_monitor = chain_mon_0;
    node_cfgs[1].chain_monitor = chain_mon_1;
    let node_chanmgrs = create_node_chanmgrs(2, &node_cfgs, &[None, None]);
    let nodes = create_network(2, &node_cfgs, &node_chanmgrs);

    // Check that the persisted channel data is empty before any channels are
    // open.
    let mut persisted_chan_data_0 = database_0
        .fetch_channel_monitors(nodes[0].keys_manager)
        .await
        .unwrap();
    assert_eq!(persisted_chan_data_0.len(), 0);
    let mut persisted_chan_data_1 = database_1
        .fetch_channel_monitors(nodes[0].keys_manager)
        .await
        .unwrap();
    assert_eq!(persisted_chan_data_1.len(), 0);

    // Helper to make sure the channel is on the expected update ID.
    macro_rules! check_persisted_data {
        ($expected_update_id: expr) => {
            persisted_chan_data_0 = database_0
                .fetch_channel_monitors(nodes[0].keys_manager)
                .await
                .unwrap();
            assert_eq!(persisted_chan_data_0.len(), 1);
            for (_, mon) in persisted_chan_data_0.iter() {
                assert_eq!(mon.get_latest_update_id(), $expected_update_id);
            }
            persisted_chan_data_1 = database_1
                .fetch_channel_monitors(nodes[0].keys_manager)
                .await
                .unwrap();
            assert_eq!(persisted_chan_data_1.len(), 1);
            for (_, mon) in persisted_chan_data_1.iter() {
                assert_eq!(mon.get_latest_update_id(), $expected_update_id);
            }
        };
    }

    // Create some initial channel and check that a channel was persisted.
    let _ = create_announced_chan_between_nodes(
        &nodes,
        0,
        1,
        channelmanager::provided_init_features(),
        channelmanager::provided_init_features(),
    );
    check_persisted_data!(0);

    // Send a few payments and make sure the monitors are updated to the latest.
    send_payment(&nodes[0], &vec![&nodes[1]][..], 8000000);
    check_persisted_data!(5);
    send_payment(&nodes[1], &vec![&nodes[0]][..], 4000000);
    check_persisted_data!(10);

    // Force close because cooperative close doesn't result in any persisted
    // updates.
    nodes[0]
        .node
        .force_close_broadcasting_latest_txn(
            &nodes[0].node.list_channels()[0].channel_id,
            &nodes[1].node.get_our_node_id(),
        )
        .unwrap();
    check_closed_event!(nodes[0], 1, ClosureReason::HolderForceClosed);
    check_closed_broadcast!(nodes[0], true);
    check_added_monitors!(nodes[0], 1);

    let node_txn = nodes[0].tx_broadcaster.txn_broadcasted.lock().unwrap();
    assert_eq!(node_txn.len(), 1);

    let header = BlockHeader {
        version: 0x20000000,
        prev_blockhash: nodes[0].best_block_hash(),
        merkle_root: TxMerkleNode::all_zeros(),
        time: 42,
        bits: 42,
        nonce: 42,
    };
    connect_block(
        &nodes[1],
        &Block {
            header,
            txdata: vec![node_txn[0].clone(), node_txn[0].clone()],
        },
    );
    check_closed_broadcast!(nodes[1], true);
    check_closed_event!(nodes[1], 1, ClosureReason::CommitmentTxConfirmed);
    check_added_monitors!(nodes[1], 1);

    // Make sure everything is persisted as expected after close.
    check_persisted_data!(11);
    teardown().await;
    teardown().await;
}

#[tokio::test(flavor = "multi_thread")]
pub async fn test_network_graph() {
    let database = setup().await;

    let network_graph = Arc::new(NetworkGraph::new(
        BlockHash::all_zeros(),
        KndLogger::global(),
    ));
    // how to make this less verbose?
    <LdkDatabase as Persister<
        '_,
        InMemorySigner,
        Arc<KndTestChainMonitor>,
        Arc<Client>,
        Arc<KeysManager>,
        Arc<Client>,
        Arc<KndLogger>,
        TestScorer,
    >>::persist_graph(&database, &network_graph)
    .unwrap();
    assert!(database.fetch_graph().await.unwrap().is_some());

    let scorer = Mutex::new(ProbabilisticScorer::new(
        ProbabilisticScoringParameters::default(),
        network_graph.clone(),
        KndLogger::global(),
    ));
    <LdkDatabase as Persister<
        '_,
        InMemorySigner,
        Arc<KndTestChainMonitor>,
        Arc<Client>,
        Arc<KeysManager>,
        Arc<Client>,
        Arc<KndLogger>,
        TestScorer,
    >>::persist_scorer(&database, &scorer)
    .unwrap();
    assert!(database
        .fetch_scorer(
            ProbabilisticScoringParameters::default(),
            network_graph.clone()
        )
        .await
        .unwrap()
        .is_some());
    teardown().await;
}

type TestScorer = Mutex<ProbabilisticScorer<Arc<NetworkGraph<Arc<KndLogger>>>, Arc<KndLogger>>>;

type KndTestChainMonitor = ChainMonitor<
    InMemorySigner,
    Arc<dyn Filter + Send + Sync>,
    Arc<Client>,
    Arc<Client>,
    Arc<KndLogger>,
    Arc<LdkDatabase>,
>;
