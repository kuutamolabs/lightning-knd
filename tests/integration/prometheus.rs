use anyhow::Result;
use futures::FutureExt;
use std::sync::Arc;
use test_utils::test_settings;

use crate::{mocks::mock_lightning::MockLightning, quit_signal};
use lightning_knd::prometheus::start_prometheus_exporter;

#[tokio::test(flavor = "multi_thread")]
pub async fn test_prometheus() {
    let address = test_settings().exporter_address.clone();
    println!("ADDRESS: {}", address);

    let metrics = Arc::new(MockLightning {
        num_nodes: 10,
        num_channels: 20,
        num_peers: 5,
        wallet_balance: 500000,
        channels: vec![],
    });
    tokio::spawn(start_prometheus_exporter(
        address.clone(),
        metrics.clone(),
        quit_signal().shared(),
    ));

    let health = call_exporter(&address, "health").await.unwrap();
    assert_eq!(health, "OK");

    let pid = call_exporter(&address, "pid").await.unwrap();
    assert_eq!(pid, std::process::id().to_string());

    let result = call_exporter(&address, "metrics").await.unwrap();
    assert!(get_metric(&result, "lightning_knd_uptime") > 0.0);
    assert_eq!(
        get_metric(&result, "lightning_node_count"),
        metrics.num_nodes as f64
    );
    assert_eq!(
        get_metric(&result, "lightning_channel_count"),
        metrics.num_channels as f64
    );
    assert_eq!(
        get_metric(&result, "lightning_peer_count"),
        metrics.num_peers as f64
    );
    assert_eq!(
        get_metric(&result, "wallet_balance"),
        metrics.wallet_balance as f64
    );

    let not_found = call_exporter(&address, "wrong").await.unwrap();
    assert_eq!(not_found, "Not Found");
}

async fn call_exporter(address: &str, method: &str) -> Result<String, reqwest::Error> {
    reqwest::get(format!("http://{}/{}", address, method))
        .await?
        .text()
        .await
}

fn get_metric(metrics: &str, name: &str) -> f64 {
    metrics
        .lines()
        .find(|x| x.starts_with(name))
        .unwrap()
        .split(' ')
        .last()
        .unwrap()
        .parse::<f64>()
        .unwrap()
}
