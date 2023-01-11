use std::thread::spawn;
use std::{fs, sync::Arc};

use axum::http::HeaderValue;
use futures::FutureExt;
use hex::ToHex;
use hyper::header::CONTENT_TYPE;
use hyper::Method;
use lightning_knd::api::start_rest_api;
use lightning_knd::api::MacaroonAuth;
use logger::KndLogger;
use once_cell::sync::Lazy;
use reqwest::RequestBuilder;
use reqwest::StatusCode;
use settings::Settings;
use test_utils::{https_client, random_public_key, TestSettingsBuilder};

use api::{routes, Balance, Channel, FundChannel, FundChannelResponse, GetInfo};
use tokio::runtime::Runtime;

use crate::mocks::mock_lightning::MockLightning;
use crate::mocks::mock_wallet::MockWallet;
use crate::quit_signal;

macro_rules! generate {
    ($name: ident, $func: expr, $method: expr, $path: expr) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            assert_eq!(
                StatusCode::UNAUTHORIZED,
                send($func($method, $path)).await.unwrap_err()
            );
        }
    };
}

generate!(
    test_root_unauthorized,
    unauthorized_request,
    Method::GET,
    routes::ROOT
);
generate!(
    test_getinfo_unauthorized,
    unauthorized_request,
    Method::GET,
    routes::GET_INFO
);
generate!(
    test_getbalance_unauthorized,
    unauthorized_request,
    Method::GET,
    routes::GET_BALANCE
);
generate!(
    test_listchannels_unauthorized,
    unauthorized_request,
    Method::GET,
    routes::LIST_CHANNELS
);
generate!(
    test_openchannel_unauthorized,
    unauthorized_request,
    Method::POST,
    routes::OPEN_CHANNEL
);

#[tokio::test(flavor = "multi_thread")]
async fn test_not_found() {
    assert_eq!(
        StatusCode::NOT_FOUND,
        send(admin_request(Method::GET, "/x")).await.unwrap_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_root_readonly() {
    assert_eq!(
        "OK",
        send(readonly_request(Method::GET, routes::ROOT))
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_root_admin() {
    assert_eq!(
        "OK",
        send(admin_request(Method::GET, routes::ROOT))
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_getinfo_readonly() {
    let result = send(readonly_request(Method::GET, routes::GET_INFO))
        .await
        .unwrap();
    let info: GetInfo = serde_json::from_str(&result).unwrap();
    assert_eq!(LIGHTNING.num_peers, info.num_peers);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_getbalance_readonly() {
    let result = send(readonly_request(Method::GET, routes::GET_BALANCE))
        .await
        .unwrap();
    let balance: Balance = serde_json::from_str(&result).unwrap();
    assert_eq!(9, balance.total_balance);
    assert_eq!(4, balance.conf_balance);
    assert_eq!(5, balance.unconf_balance);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_listchannels_readonly() {
    let result = send(readonly_request(Method::GET, routes::LIST_CHANNELS))
        .await
        .unwrap();
    let channels: Vec<Channel> = serde_json::from_str(&result).unwrap();
    let channel = channels.get(0).unwrap();
    assert_eq!(
        "0202755b475334bd9a56a317fd23dfe264b193bcbd7322faa3e974031704068266",
        channel.id
    );
    assert_eq!("true", channel.connected);
    assert_eq!("usable", channel.state);
    assert_eq!("34234124", channel.short_channel_id);
    assert_eq!(
        "0000000000000000000000000000000000000000000000000000000000000000",
        channel.funding_txid
    );
    assert_eq!("false", channel.private);
    assert_eq!("", channel.msatoshi_to_us);
    assert_eq!("1000000", channel.msatoshi_total);
    assert_eq!("", channel.msatoshi_to_them);
    assert_eq!("5000", channel.their_channel_reserve_satoshis);
    assert_eq!("10000", channel.our_channel_reserve_satoshis);
    assert_eq!("100000", channel.spendable_msatoshi);
    assert_eq!(1, channel.direction);
    assert_eq!("test_node                       ", channel.alias);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_openchannel_readonly() {
    let request = fund_channel_request();
    let body = serde_json::to_string(&request).unwrap();
    let result = send(readonly_request(Method::POST, routes::OPEN_CHANNEL).body(body))
        .await
        .unwrap_err();
    assert_eq!(StatusCode::UNAUTHORIZED, result)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_openchannel_admin() {
    let request = fund_channel_request();
    let body = serde_json::to_string(&request).unwrap();
    let result = send(admin_request(Method::POST, routes::OPEN_CHANNEL).body(body))
        .await
        .unwrap();
    let response: FundChannelResponse = serde_json::from_str(&result).unwrap();
    assert_eq!(
        "fba98a9a61ef62c081b31769f66a81f1640b4f94d48b550a550034cb4990eded",
        response.txid
    );
    assert_eq!(
        "0101010101010101010101010101010101010101010101010101010101010101",
        response.channel_id
    );
}

fn fund_channel_request() -> FundChannel {
    FundChannel {
        id: random_public_key().serialize().encode_hex(),
        satoshis: "21000000".to_string(),
        fee_rate: "4".to_string(),
        announce: "true".to_string(),
        push_msat: "10000".to_string(),
        close_to: "".to_string(),
        request_amt: "".to_string(),
        compact_lease: "".to_string(),
        min_conf: 5,
        utxos: vec![],
    }
}

static API_RUNTIME: Lazy<Runtime> = Lazy::new(|| Runtime::new().unwrap());

pub static API_SETTINGS: Lazy<Settings> = Lazy::new(|| {
    KndLogger::init("test", log::LevelFilter::Info);
    let settings = TestSettingsBuilder::new()
        .with_data_dir(&format!("{}/test_api", env!("CARGO_TARGET_TMPDIR")))
        .build();
    let macaroon_auth = Arc::new(MacaroonAuth::init(&[0u8; 32], &settings.data_dir).unwrap());

    let settings_clone = settings.clone();
    // Run the API with its own runtime in its own thread.
    spawn(move || {
        API_RUNTIME
            .block_on(start_rest_api(
                settings_clone.rest_api_address.clone(),
                settings_clone.certs_dir.clone(),
                LIGHTNING.clone(),
                Arc::new(MockWallet::default()),
                macaroon_auth,
                quit_signal().shared(),
            ))
            .unwrap()
    });

    settings
});

static ADMIN_MACAROON: Lazy<Vec<u8>> = Lazy::new(|| {
    fs::read(format!(
        "{}/macaroons/admin_macaroon",
        API_SETTINGS.data_dir
    ))
    .unwrap()
});

static READONLY_MACAROON: Lazy<Vec<u8>> = Lazy::new(|| {
    fs::read(format!(
        "{}/macaroons/readonly_macaroon",
        API_SETTINGS.data_dir
    ))
    .unwrap()
});

static LIGHTNING: Lazy<Arc<MockLightning>> = Lazy::new(|| Arc::new(MockLightning::default()));

fn unauthorized_request(method: Method, route: &str) -> RequestBuilder {
    let address = &API_SETTINGS.rest_api_address;
    https_client()
        .request(method, format!("https://{}{}", address, route))
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
}

fn admin_request(method: Method, route: &str) -> RequestBuilder {
    unauthorized_request(method, route).header("macaroon", ADMIN_MACAROON.to_owned())
}

fn readonly_request(method: Method, route: &str) -> RequestBuilder {
    unauthorized_request(method, route).header("macaroon", READONLY_MACAROON.to_owned())
}

async fn send(builder: RequestBuilder) -> Result<String, StatusCode> {
    let response = builder.send().await.unwrap();
    if !response.status().is_success() {
        return Err(response.status());
    }
    Ok(response.text().await.unwrap())
}
