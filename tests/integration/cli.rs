use std::process::Command;

use api::GetInfo;

use super::api::API_SETTINGS;

#[test]
fn test_cli_get_info() {
    let settings = &API_SETTINGS;

    let output = Command::new(env!("CARGO_BIN_EXE_lightning-knd-cli"))
        .args([
            "--target",
            &settings.rest_api_address,
            "--cert-path",
            &format!("{}/knd.crt", settings.certs_dir),
            "--macaroon-path",
            &format!("{}/macaroons/admin_macaroon", settings.data_dir),
            "get-info",
        ])
        .output()
        .unwrap();

    let _: GetInfo = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success());
}
