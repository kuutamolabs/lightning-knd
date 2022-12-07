use crate::cipher::Cipher;
use crate::payment::{Payment, MillisatAmount};
use crate::{connection, to_i64, Client};
use anyhow::{bail, Result};
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::Hash;
use bitcoin::secp256k1::PublicKey;
use bitcoin::{BlockHash, Txid};
use lightning::chain::chaininterface::{BroadcasterInterface, FeeEstimator};
use lightning::chain::chainmonitor::MonitorUpdateId;
use lightning::chain::channelmonitor::{ChannelMonitor, ChannelMonitorUpdate};
use lightning::chain::keysinterface::{KeysInterface, Sign};
use lightning::chain::transaction::OutPoint;
use lightning::chain::{self, ChannelMonitorUpdateStatus, Watch};
use lightning::ln::channelmanager::{ChannelManager, ChannelManagerReadArgs};
use lightning::ln::{PaymentHash, PaymentPreimage, PaymentSecret};
use lightning::routing::gossip::NetworkGraph;
use lightning::routing::scoring::{
    ProbabilisticScorer, ProbabilisticScoringParameters, WriteableScore,
};
use lightning::util::logger::Logger;
use lightning::util::persist::Persister;
use lightning::util::ser::ReadableArgs;
use lightning::util::ser::Writeable;
use log::{debug, info};
use logger::KndLogger;
use settings::Settings;
use std::convert::TryInto;
use std::io::Cursor;
use std::ops::Deref;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::sync::RwLock;

use crate::peer::Peer;

macro_rules! block_in_place {
    ($statement: literal, $params: expr, $self: expr) => {
        tokio::task::block_in_place(move || {
            Handle::current().block_on(async move {
                $self
                    .client
                    .read()
                    .await
                    .execute($statement, $params)
                    .await
                    .unwrap()
            })
        })
    };
}

pub struct LdkDatabase {
    pub(crate) client: Arc<RwLock<Client>>,
    cipher: Cipher,
}

impl LdkDatabase {
    pub async fn new(settings: &Settings) -> Result<LdkDatabase> {
        info!(
            "Connecting LDK to Cockroach database at {}:{}",
            settings.database_host, settings.database_port
        );
        let client = connection(&settings).await?;
        let client = Arc::new(RwLock::new(client));

        let cipher = Cipher::new(&settings);

        Ok(LdkDatabase { client, cipher })
    }

    pub async fn is_first_start(&self) -> Result<bool> {
        Ok(self
            .client
            .read()
            .await
            .query_opt("SELECT true FROM keys", &[])
            .await?
            .is_none())
    }

    pub async fn persist_keys(&self, public: &PublicKey, seed: &[u8; 32]) -> Result<()> {
        debug!("Persist keys: {}", public);
        let ciphertext = self.cipher.encrypt(seed);
        self.client
            .read()
            .await
            .execute(
                "INSERT INTO keys (public_key, seed) VALUES ($1, $2)",
                &[&public.serialize().to_vec(), &ciphertext],
            )
            .await?;
        Ok(())
    }

    pub async fn fetch_keys(&self) -> Result<(PublicKey, [u8; 32])> {
        debug!("Fetch keys");
        let row = self
            .client
            .read()
            .await
            .query_one("SELECT public_key, seed FROM keys", &[])
            .await?;
        let bytes: Vec<u8> = row.get(0);
        let public = PublicKey::from_slice(&bytes)?;
        let ciphertext: Vec<u8> = row.get(1);
        let bytes = self.cipher.decrypt(&ciphertext);
        let seed = bytes.try_into().expect("Seed is the wrong length");
        Ok((public, seed))
    }

    pub async fn persist_peer(&self, peer: &Peer) -> Result<()> {
        self.client
            .read()
            .await
            .execute(
                "UPSERT INTO peers (public_key, address) \
            VALUES ($1, $2)",
                &[
                    &peer.public_key.encode().as_slice(),
                    &peer.socket_addr.to_string().as_bytes(),
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn fetch_peers(&self) -> Result<Vec<Peer>> {
        debug!("Fetching peers from database");
        let mut peers = Vec::new();
        for row in self
            .client
            .read()
            .await
            .query("SELECT * FROM peers", &[])
            .await?
        {
            let public_key: Vec<u8> = row.get(&"public_key");
            let address: Vec<u8> = row.get(&"address");
            peers.push(Peer {
                public_key: PublicKey::from_slice(&public_key).unwrap(),
                socket_addr: String::from_utf8(address)?.parse().unwrap(),
            });
        }
        debug!("Fetched {} peers", peers.len());
        Ok(peers)
    }

    pub async fn delete_peer(&self, peer: &Peer) {
        self.client
            .read()
            .await
            .execute(
                "DELETE FROM peers \
            WHERE public_key = $1 AND address = $2",
                &[
                    &peer.public_key.encode(),
                    &peer.socket_addr.to_string().as_bytes(),
                ],
            )
            .await
            .unwrap();
    }

    pub async fn fetch_channel_monitors<Signer: Sign, K: Deref>(
        &self,
        keys_manager: K,
        //		broadcaster: &B,
        //		fee_estimator: &F,
    ) -> Result<Vec<(BlockHash, ChannelMonitor<Signer>)>>
    where
        <K as Deref>::Target: KeysInterface<Signer = Signer> + Sized,
        //      B::Target: BroadcasterInterface,
        //		F::Target: FeeEstimator,
    {
        let rows = self
            .client
            .read()
            .await
            .query(
                "SELECT out_point, monitor \
            FROM channel_monitors",
                &[],
            )
            .await
            .unwrap();
        let mut monitors: Vec<(BlockHash, ChannelMonitor<Signer>)> = vec![];
        for row in rows {
            let out_point: Vec<u8> = row.get("out_point");

            let (txid_bytes, index_bytes) = out_point.split_at(32);
            let txid = Txid::from_slice(txid_bytes).unwrap();
            let index = u16::from_le_bytes(index_bytes.try_into().unwrap());

            let ciphertext: Vec<u8> = row.get("monitor");
            let bytes = self.cipher.decrypt(&ciphertext);
            let mut buffer = Cursor::new(&bytes);
            match <(BlockHash, ChannelMonitor<Signer>)>::read(&mut buffer, &*keys_manager) {
                Ok((blockhash, channel_monitor)) => {
                    if channel_monitor.get_funding_txo().0.txid != txid
                        || channel_monitor.get_funding_txo().0.index != index
                    {
                        bail!("Unable to find ChannelMonitor for: {}:{}", txid, index);
                    }
                    /*
                                        let update_rows = self
                                            .client
                                            .read()
                                            .await
                                            .query(
                                                "SELECT update \
                                            FROM channel_monitor_updates \
                                            WHERE out_point = $1 \
                                            ORDER BY update_id ASC",
                                                &[&out_point],
                                            )
                                            .await
                                            .unwrap();

                                        let updates: Vec<ChannelMonitorUpdate> = update_rows
                                            .iter()
                                            .map(|row| {
                                                let ciphertext: Vec<u8> = row.get("update");
                                                let update = self.cipher.decrypt(&ciphertext);
                                                ChannelMonitorUpdate::read(&mut Cursor::new(&update)).unwrap()
                                            })
                                            .collect();
                                        for update in updates {
                                            channel_monitor
                                                .update_monitor(&update, broadcaster, fee_estimator.clone(), &KndLogger::global()).unwrap();
                                        }
                    */
                    monitors.push((blockhash, channel_monitor));
                }
                Err(e) => bail!("Failed to deserialize ChannelMonitor: {}", e),
            }
        }
        Ok(monitors)
    }

    pub async fn fetch_channel_manager<
        Signer: Sign,
        M: Deref,
        T: Deref,
        K: Deref,
        F: Deref,
        L: Deref,
    >(
        &self,
        read_args: ChannelManagerReadArgs<'_, Signer, M, T, K, F, L>,
    ) -> Result<(BlockHash, ChannelManager<Signer, M, T, K, F, L>)>
    where
        <M as Deref>::Target: Watch<Signer>,
        <T as Deref>::Target: BroadcasterInterface,
        <K as Deref>::Target: KeysInterface<Signer = Signer>,
        <F as Deref>::Target: FeeEstimator,
        <L as Deref>::Target: Logger,
    {
        let row = self
            .client
            .read()
            .await
            .query_one(
                "SELECT manager \
            FROM channel_manager",
                &[],
            )
            .await?;
        let ciphertext: Vec<u8> = row.get("manager");
        let bytes = self.cipher.decrypt(&ciphertext);
        Ok(<(BlockHash, ChannelManager<Signer, M, T, K, F, L>)>::read(
            &mut Cursor::new(bytes),
            read_args,
        )
        .unwrap())
    }

    pub async fn fetch_graph(&self) -> Result<Option<NetworkGraph<Arc<KndLogger>>>> {
        let graph = self
            .client
            .read()
            .await
            .query_opt("SELECT graph FROM network_graph", &[])
            .await?
            .map(|row| {
                let bytes: Vec<u8> = row.get(0);
                NetworkGraph::read(&mut Cursor::new(bytes), KndLogger::global())
                    .expect("Unable to deserialize network graph")
            });
        Ok(graph)
    }

    pub async fn fetch_scorer(
        &self,
        params: ProbabilisticScoringParameters,
        graph: Arc<NetworkGraph<Arc<KndLogger>>>,
    ) -> Result<Option<ProbabilisticScorer<Arc<NetworkGraph<Arc<KndLogger>>>, Arc<KndLogger>>>>
    {
        let scorer = self
            .client
            .read()
            .await
            .query_opt("SELECT scorer FROM scorer", &[])
            .await?
            .map(|row| {
                let bytes: Vec<u8> = row.get(0);
                ProbabilisticScorer::read(
                    &mut Cursor::new(bytes),
                    (params.clone(), graph.clone(), KndLogger::global()),
                )
                .expect("Unable to deserialize scorer")
            });
        Ok(scorer)
    }

    pub async fn persist_payment(&self, hash: &PaymentHash, payment: &Payment) -> Result<()> {
        debug!("Persist payment: {}", serialize_hex(&hash.0));
        let ciphertext = payment.secret.map(|s| self.cipher.encrypt(&s.0));
        self.client
            .read()
            .await
            .execute(
                "UPSERT INTO payments (hash, preimage, secret, status, amount_msat, is_outbound) VALUES ($1, $2, $3, $4, $5, $6)",
                &[&hash.0.as_ref(), &payment.preimage.as_ref().map(|x| x.0.as_ref()), &ciphertext, &payment.status, &payment.amount_msat.0, &payment.is_outbound],
            )
            .await?;
        Ok(())
    }

    pub async fn fetch_payment(&self, hash: &PaymentHash) -> Result<Option<Payment>> {
        let payment = self.client.read().await.query_opt("SELECT hash, preimage, secret, status, amount_msat, is_outbound, FROM payments WHERE hash = $1", &[&hash.0.as_ref()])
        .await?
        .map(|row| {
            let preimage: Option<Vec<u8>> = row.get("preimage");
            let ciphertext: Option<Vec<u8>> = row.get("secret");
            let secret = ciphertext.map(|c| self.cipher.decrypt(&c));

            Payment {
                preimage: preimage.map(|p| PaymentPreimage(p.try_into().unwrap())),
                secret: secret.map(|s| PaymentSecret(s.try_into().unwrap())),
                status: row.get("status"),
                amount_msat: MillisatAmount(row.get("amount_msat")),
                is_outbound: row.get("is_outbound")
            }
        });
        Ok(payment)
    }
}

impl<'a, Signer: Sign, M: Deref, T: Deref, K: Deref, F: Deref, L: Deref, S>
    Persister<'a, Signer, M, T, K, F, L, S> for LdkDatabase
where
    M::Target: 'static + chain::Watch<Signer>,
    T::Target: 'static + BroadcasterInterface,
    K::Target: 'static + KeysInterface<Signer = Signer>,
    F::Target: 'static + FeeEstimator,
    L::Target: 'static + Logger,
    S: WriteableScore<'a>,
{
    fn persist_manager(
        &self,
        channel_manager: &ChannelManager<Signer, M, T, K, F, L>,
    ) -> Result<(), std::io::Error> {
        let mut buf = vec![];
        channel_manager.write(&mut buf).unwrap();
        let ciphertext = self.cipher.encrypt(&buf);
        block_in_place!(
            "UPSERT INTO channel_manager (manager, timestamp) \
            VALUES ($1, CURRENT_TIMESTAMP)",
            &[&ciphertext],
            self
        );
        Ok(())
    }

    fn persist_graph(
        &self,
        network_graph: &lightning::routing::gossip::NetworkGraph<L>,
    ) -> Result<(), std::io::Error> {
        let mut buf = vec![];
        network_graph.write(&mut buf).unwrap();
        block_in_place!(
            "UPSERT INTO network_graph (graph, timestamp)
            VALUES ($1, CURRENT_TIMESTAMP)",
            &[&buf],
            self
        );
        Ok(())
    }

    fn persist_scorer(&self, scorer: &S) -> Result<(), std::io::Error> {
        let mut buf = vec![];
        scorer.write(&mut buf).unwrap();
        block_in_place!(
            "UPSERT INTO scorer (scorer, timestamp)
            VALUES ($1, CURRENT_TIMESTAMP)",
            &[&buf],
            self
        );
        Ok(())
    }
}

impl<ChannelSigner: Sign> chain::chainmonitor::Persist<ChannelSigner> for LdkDatabase {
    // The CHANNEL_MONITORS table stores the latest monitor and its update_id.
    fn persist_new_channel(
        &self,
        funding_txo: OutPoint,
        monitor: &ChannelMonitor<ChannelSigner>,
        _update_id: MonitorUpdateId,
    ) -> ChannelMonitorUpdateStatus {
        debug!(
            "Persisting new channel: {:?}:{}",
            funding_txo,
            monitor.get_latest_update_id()
        );

        let mut out_point_buf = vec![];
        funding_txo.write(&mut out_point_buf).unwrap();

        let mut monitor_buf = vec![];
        monitor.write(&mut monitor_buf).unwrap();
        let ciphertext = self.cipher.encrypt(&monitor_buf);

        block_in_place!(
            "UPSERT INTO channel_monitors (out_point, monitor, update_id) \
            VALUES ($1, $2, $3)",
            &[
                &out_point_buf,
                &ciphertext,
                &to_i64!(monitor.get_latest_update_id())
            ],
            self
        );
        ChannelMonitorUpdateStatus::Completed
    }

    // Updates are applied to the monitor when fetched from database.
    fn update_persisted_channel(
        &self,
        funding_txo: OutPoint,
        _update: &Option<ChannelMonitorUpdate>,
        monitor: &ChannelMonitor<ChannelSigner>,
        update_id: MonitorUpdateId, // only need this if persisting async.
    ) -> ChannelMonitorUpdateStatus {
        debug!(
            "Updating persisted channel: {:?}:{}",
            funding_txo,
            monitor.get_latest_update_id()
        );
        self.persist_new_channel(funding_txo, monitor, update_id)

        // Hope we can enable this soon. Probably after https://github.com/lightningdevkit/rust-lightning/issues/1426
        /*
                let mut out_point_buf = vec![];
                funding_txo.write(&mut out_point_buf).unwrap();

                // If its the last update then store the last monitor and delete the updates.
                if update.as_ref().map_or(true, |x| x.update_id == CLOSED_CHANNEL_UPDATE_ID) {
                    let mut monitor_buf = vec![];
                    monitor.write(&mut monitor_buf).unwrap();
                    let ciphertext = self.cipher.encrypt(&monitor_buf);

                    tokio::task::block_in_place(move || {
                        Handle::current().block_on(async move {
                            let mut lock = self.client.write().await;
                            let tx = lock.transaction().await.unwrap();
                            tx.execute(
                                "UPSERT INTO channel_monitors (out_point, monitor, update_id) VALUES ($1, $2, $3)",
                                &[
                                    &out_point_buf,
                                    &ciphertext,
                                    &to_i64!(monitor.get_latest_update_id()),
                                ],
                            )
                            .await
                            .unwrap();
                            let deleted = tx
                                .execute(
                                    "DELETE FROM channel_monitor_updates WHERE out_point = $1",
                                    &[&out_point_buf],
                                )
                                .await
                                .unwrap();
                            tx.commit().await.unwrap();
                            debug!("Stored latest monitor and deleted {} updates.", deleted);
                        })
                    })
                } else {
                    let update = update.as_ref().unwrap();
                    let mut update_buf = vec![];
                    update.write(&mut update_buf).unwrap();
                    let ciphertext = self.cipher.encrypt(&update_buf);

                    block_in_place!(
                        "UPSERT INTO channel_monitor_updates (out_point, update, update_id) \
                        VALUES ($1, $2, $3)",
                        &[&out_point_buf, &ciphertext, &to_i64!(update.update_id)],
                        self
                    );
                }
                ChannelMonitorUpdateStatus::Completed
        */
    }
}
