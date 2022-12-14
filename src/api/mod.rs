mod channels;
mod lightning_interface;
mod macaroon_auth;
mod methods;
mod wallet;
mod wallet_interface;

pub use lightning_interface::{LightningInterface, OpenChannelResult};
pub use macaroon_auth::{KndMacaroon, MacaroonAuth};
pub use wallet_interface::WalletInterface;

use self::methods::get_info;
use crate::api::{
    channels::{list_channels, open_channel},
    wallet::get_balance,
};
use anyhow::Result;
use api::routes;
use axum::{
    extract::Extension,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use axum_server::{tls_rustls::RustlsConfig, Handle};
use futures::{future::Shared, Future};
use hyper::StatusCode;
use log::{error, info};
use std::{sync::Arc, time::Duration};
use tower_http::cors::CorsLayer;

pub async fn start_rest_api(
    listen_address: String,
    certs_dir: String,
    lightning_api: Arc<dyn LightningInterface + Send + Sync>,
    wallet_api: Arc<dyn WalletInterface + Send + Sync>,
    macaroon_auth: Arc<MacaroonAuth>,
    quit_signal: Shared<impl Future<Output = ()>>,
) -> Result<()> {
    info!("Starting REST API");
    let rustls_config = config(&certs_dir).await;
    let cors = CorsLayer::permissive();
    let handle = Handle::new();

    let app = Router::new()
        .route(routes::ROOT, get(root))
        .route(routes::GET_INFO, get(get_info))
        .route(routes::GET_BALANCE, get(get_balance))
        .route(routes::LIST_CHANNELS, get(list_channels))
        .route(routes::OPEN_CHANNEL, post(open_channel))
        .fallback(handler_404)
        .layer(cors)
        .layer(Extension(lightning_api))
        .layer(Extension(wallet_api))
        .layer(Extension(macaroon_auth));

    let addr = listen_address.parse()?;

    tokio::select!(
        result = axum_server::bind_rustls(addr, rustls_config)
            .serve(app.into_make_service()) => {
                if let Err(e) = result {
                    error!("API server shutdown unexpectedly: {}", e);
                } else {
                    info!("API server shutdown successfully.");
                }
        }
        _ = quit_signal => {
            handle.graceful_shutdown(Some(Duration::from_secs(30)));
        }
    );
    Ok(())
}

async fn root(
    macaroon: KndMacaroon,
    Extension(macaroon_auth): Extension<Arc<MacaroonAuth>>,
) -> Result<impl IntoResponse, StatusCode> {
    if macaroon_auth.verify_readonly_macaroon(&macaroon.0).is_err() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok("OK")
}

async fn handler_404() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "No such method.")
}

async fn config(certs_dir: &str) -> RustlsConfig {
    RustlsConfig::from_pem_file(
        format!("{}/knd.crt", certs_dir),
        format!("{}/knd.key", certs_dir),
    )
    .await
    .unwrap()
}

#[macro_export]
macro_rules! to_string_empty {
    ($v: expr) => {
        $v.map_or("".to_string(), |x| x.to_string())
    };
}

#[macro_export]
macro_rules! handle_err {
    ($parse:expr) => {
        $parse.map_err(|e| {
            warn!("{}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })
    };
}

#[macro_export]
macro_rules! handle_auth_err {
    ($parse:expr) => {
        $parse.map_err(|e| {
            info!("{}", e);
            StatusCode::UNAUTHORIZED
        })
    };
}
