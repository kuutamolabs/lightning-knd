use tokio::signal::unix::SignalKind;

mod integration;
pub mod mocks;

pub async fn quit_signal() {
    let _ = tokio::signal::unix::signal(SignalKind::quit())
        .unwrap()
        .recv()
        .await;
}
