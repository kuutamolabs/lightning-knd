use std::{fs::File, io::Read};

use anyhow::{anyhow, Result};
use api::routes;
use reqwest::{
    blocking::{Client, ClientBuilder, RequestBuilder},
    header::{HeaderValue, CONTENT_TYPE},
    Certificate, Method,
};

pub struct Api {
    host: String,
    client: Client,
    macaroon: Vec<u8>,
}

impl Api {
    pub fn new(host: &str, cert_path: &str, macaroon_path: &str) -> Result<Api> {
        let macaroon = read_file(macaroon_path)?;
        let cert = Certificate::from_pem(&read_file(cert_path)?)?;
        // Rustls does not support IP addresses (hostnames only) to we need to use native tls (openssl). Also turn off SNI as this requires host names as well.
        let client = ClientBuilder::new()
            .tls_sni(false)
            .add_root_certificate(cert)
            .use_native_tls()
            .build()?;
        Ok(Api {
            host: host.to_string(),
            client,
            macaroon,
        })
    }

    pub fn get_info(&self) -> Result<String> {
        let response = self.request(Method::GET, routes::GET_INFO).send()?;
        if !response.status().is_success() {
            return Err(anyhow!("{}", response.status()));
        }
        Ok(response.text()?)
    }

    fn request(&self, method: Method, route: &str) -> RequestBuilder {
        self.client
            .request(method, format!("https://{}{}", self.host, route))
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header("macaroon", self.macaroon.clone())
    }
}

fn read_file(path: &str) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match File::open(path) {
        Ok(mut file) => match file.read_to_end(&mut buf) {
            Ok(_) => Ok(buf),
            Err(e) => Err(anyhow!("{}: {}", e, path)),
        },
        Err(e) => Err(anyhow!("{}: {}", e, path)),
    }
}
