use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bitcoin::network::constants::Network;
use bitcoin::secp256k1::Secp256k1;
use bitcoin_bech32::WitnessProgram;
use hex::ToHex;
use lightning::chain::chaininterface::{BroadcasterInterface, ConfirmationTarget, FeeEstimator};
use lightning::chain::keysinterface::KeysManager;
use lightning::routing::gossip::NodeId;
use lightning::util::events::{Event, PaymentPurpose};
use log::{error, info};
use rand::{thread_rng, Rng};
use tokio::runtime::Handle;

use crate::controller::{AsyncAPIRequests, ChannelManager, NetworkGraph};
use crate::payment_info::{HTLCStatus, MillisatAmount, PaymentInfo, PaymentInfoStorage};
use crate::wallet::Wallet;
use bitcoind::Client;

pub(crate) struct EventHandler {
    channel_manager: Arc<ChannelManager>,
    bitcoind_client: Arc<Client>,
    keys_manager: Arc<KeysManager>,
    inbound_payments: PaymentInfoStorage,
    outbound_payments: PaymentInfoStorage,
    network: Network,
    network_graph: Arc<NetworkGraph>,
    wallet: Arc<Wallet>,
    async_api_requests: Arc<AsyncAPIRequests>,
}

impl EventHandler {
    // TODO remove when payments storage is in database
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channel_manager: Arc<ChannelManager>,
        bitcoind_client: Arc<Client>,
        keys_manager: Arc<KeysManager>,
        inbound_payments: PaymentInfoStorage,
        outbound_payments: PaymentInfoStorage,
        network: Network,
        network_graph: Arc<NetworkGraph>,
        wallet: Arc<Wallet>,
        async_api_requests: Arc<AsyncAPIRequests>,
    ) -> EventHandler {
        EventHandler {
            channel_manager,
            bitcoind_client,
            keys_manager,
            inbound_payments,
            outbound_payments,
            network,
            network_graph,
            wallet,
            async_api_requests,
        }
    }
}

impl lightning::util::events::EventHandler for EventHandler {
    fn handle_event(&self, event: lightning::util::events::Event) {
        tokio::task::block_in_place(move || {
            Handle::current().block_on(self.handle_event_async(event))
        })
    }
}

impl EventHandler {
    pub async fn handle_event_async(&self, event: lightning::util::events::Event) {
        match event {
            Event::FundingGenerationReady {
                temporary_channel_id,
                counterparty_node_id,
                channel_value_satoshis,
                output_script,
                user_channel_id,
            } => {
                // Construct the raw transaction with one output, that is paid the amount of the
                // channel.
                let addr = WitnessProgram::from_scriptpubkey(
                    &output_script[..],
                    match self.network {
                        Network::Bitcoin => bitcoin_bech32::constants::Network::Bitcoin,
                        Network::Testnet => bitcoin_bech32::constants::Network::Testnet,
                        Network::Regtest => bitcoin_bech32::constants::Network::Regtest,
                        Network::Signet => bitcoin_bech32::constants::Network::Signet,
                    },
                )
                .expect("Lightning funding tx should always be to a SegWit output")
                .to_address();
                let mut outputs = vec![HashMap::with_capacity(1)];
                outputs[0].insert(addr, channel_value_satoshis as f64 / 100_000_000.0);

                let funding_tx = self
                    .wallet
                    .fund_tx(&output_script, &channel_value_satoshis)
                    .unwrap();

                // Give the funding transaction back to LDK for opening the channel.
                if self
                    .channel_manager
                    .funding_transaction_generated(
                        &temporary_channel_id,
                        &counterparty_node_id,
                        funding_tx.clone(),
                    )
                    .is_err()
                {
                    error!("Channel went away before we could fund it. The peer disconnected or refused the channel.");
                }
                self.async_api_requests
                    .channel_opens
                    .send(user_channel_id, funding_tx)
                    .await;
            }
            Event::ChannelReady {
                channel_id: _,
                user_channel_id: _,
                counterparty_node_id: _,
                channel_type: _,
            } => {}
            Event::ChannelClosed {
                channel_id,
                reason,
                user_channel_id: _,
            } => {
                info!("EVENT: Channel {:?} closed due to: {}", channel_id, reason);
            }
            Event::DiscardFunding { .. } => {
                // A "real" node should probably "lock" the UTXOs spent in funding transactions until
                // the funding transaction either confirms, or this event is generated.
            }
            Event::OpenChannelRequest { .. } => {
                // Unreachable, we don't set manually_accept_inbound_channels
            }
            Event::PaymentClaimable {
                payment_hash,
                purpose,
                amount_msat,
                receiver_node_id: _,
                via_channel_id: _,
                via_user_channel_id: _,
            } => {
                info!(
                    "EVENT: received payment from payment hash {} of {} millisatoshis",
                    payment_hash.0.encode_hex::<String>(),
                    amount_msat,
                );
                let payment_preimage = match purpose {
                    PaymentPurpose::InvoicePayment {
                        payment_preimage, ..
                    } => payment_preimage,
                    PaymentPurpose::SpontaneousPayment(preimage) => Some(preimage),
                };
                self.channel_manager.claim_funds(payment_preimage.unwrap());
            }
            Event::PaymentClaimed {
                payment_hash,
                purpose,
                amount_msat,
                receiver_node_id: _,
            } => {
                info!(
                    "EVENT: claimed payment from payment hash {} of {} millisatoshis",
                    payment_hash.0.encode_hex::<String>(),
                    amount_msat,
                );
                let (payment_preimage, payment_secret) = match purpose {
                    PaymentPurpose::InvoicePayment {
                        payment_preimage,
                        payment_secret,
                        ..
                    } => (payment_preimage, Some(payment_secret)),
                    PaymentPurpose::SpontaneousPayment(preimage) => (Some(preimage), None),
                };
                let mut payments = self.inbound_payments.lock().unwrap();
                match payments.entry(payment_hash) {
                    Entry::Occupied(mut e) => {
                        let payment = e.get_mut();
                        payment.status = HTLCStatus::Succeeded;
                        payment.preimage = payment_preimage;
                        payment.secret = payment_secret;
                    }
                    Entry::Vacant(e) => {
                        e.insert(PaymentInfo {
                            preimage: payment_preimage,
                            secret: payment_secret,
                            status: HTLCStatus::Succeeded,
                            amt_msat: MillisatAmount(Some(amount_msat)),
                        });
                    }
                }
            }
            Event::PaymentSent {
                payment_preimage,
                payment_hash,
                fee_paid_msat,
                ..
            } => {
                let mut payments = self.outbound_payments.lock().unwrap();
                if let Some(payment) = payments.get_mut(&payment_hash) {
                    payment.preimage = Some(payment_preimage);
                    payment.status = HTLCStatus::Succeeded;
                    info!(
                        "EVENT: successfully sent payment of {} millisatoshis{} from \
								 payment hash {:?} with preimage {:?}",
                        payment.amt_msat,
                        if let Some(fee) = fee_paid_msat {
                            format!(" (fee {} msat)", fee)
                        } else {
                            "".to_string()
                        },
                        payment_hash.0.encode_hex::<String>(),
                        payment_preimage.0.encode_hex::<String>()
                    );
                }
            }
            Event::PaymentPathSuccessful { .. } => {}
            Event::PaymentPathFailed { .. } => {}
            Event::ProbeSuccessful { .. } => {}
            Event::ProbeFailed { .. } => {}
            Event::PaymentFailed { payment_hash, .. } => {
                info!(
				"EVENT: Failed to send payment to payment hash {:?}: exhausted payment retry attempts",
				payment_hash.0.encode_hex::<String>()
			);

                let mut payments = self.outbound_payments.lock().unwrap();
                if payments.contains_key(&payment_hash) {
                    let payment = payments.get_mut(&payment_hash).unwrap();
                    payment.status = HTLCStatus::Failed;
                }
            }
            Event::PaymentForwarded {
                prev_channel_id,
                next_channel_id,
                fee_earned_msat,
                claim_from_onchain_tx,
            } => {
                let read_only_network_graph = self.network_graph.read_only();
                let nodes = read_only_network_graph.nodes();
                let channels = self.channel_manager.list_channels();

                let node_str = |channel_id: &Option<[u8; 32]>| match channel_id {
                    None => String::new(),
                    Some(channel_id) => match channels.iter().find(|c| c.channel_id == *channel_id)
                    {
                        None => String::new(),
                        Some(channel) => {
                            match nodes.get(&NodeId::from_pubkey(&channel.counterparty.node_id)) {
                                None => "private node".to_string(),
                                Some(node) => match &node.announcement_info {
                                    None => "unnamed node".to_string(),
                                    Some(announcement) => {
                                        format!("node {}", announcement.alias)
                                    }
                                },
                            }
                        }
                    },
                };
                let channel_str = |channel_id: &Option<[u8; 32]>| {
                    channel_id
                        .map(|channel_id| format!(" with channel {:?}", channel_id))
                        .unwrap_or_default()
                };
                let from_prev_str = format!(
                    " from {}{}",
                    node_str(&prev_channel_id),
                    channel_str(&prev_channel_id)
                );
                let to_next_str = format!(
                    " to {}{}",
                    node_str(&next_channel_id),
                    channel_str(&next_channel_id)
                );

                let from_onchain_str = if claim_from_onchain_tx {
                    "from onchain downstream claim"
                } else {
                    "from HTLC fulfill message"
                };
                if let Some(fee_earned) = fee_earned_msat {
                    info!(
                        "EVENT: Forwarded payment{}{}, earning {} msat {}",
                        from_prev_str, to_next_str, fee_earned, from_onchain_str
                    );
                } else {
                    info!(
                        "EVENT: Forwarded payment{}{}, claiming onchain {}",
                        from_prev_str, to_next_str, from_onchain_str
                    );
                }
            }
            Event::HTLCHandlingFailed { .. } => {}
            Event::PendingHTLCsForwardable { time_forwardable } => {
                let forwarding_channel_manager = self.channel_manager.clone();
                let min = time_forwardable.as_millis() as u64;
                tokio::spawn(async move {
                    let millis_to_sleep = thread_rng().gen_range(min..min * 5) as u64;
                    tokio::time::sleep(Duration::from_millis(millis_to_sleep)).await;
                    forwarding_channel_manager.process_pending_htlc_forwards();
                });
            }
            Event::SpendableOutputs { outputs } => {
                let destination_address = self.wallet.get_new_address().unwrap();
                let output_descriptors = &outputs.iter().collect::<Vec<_>>();
                let tx_feerate = self
                    .bitcoind_client
                    .get_est_sat_per_1000_weight(ConfirmationTarget::Normal);
                let spending_tx = self
                    .keys_manager
                    .spend_spendable_outputs(
                        output_descriptors,
                        Vec::new(),
                        destination_address.script_pubkey(),
                        tx_feerate,
                        &Secp256k1::new(),
                    )
                    .unwrap();
                self.bitcoind_client.broadcast_transaction(&spending_tx);
            }
            Event::HTLCIntercepted {
                intercept_id: _,
                requested_next_hop_scid: _,
                payment_hash: _,
                inbound_amount_msat: _,
                expected_outbound_amount_msat: _,
            } => {}
        }
    }
}
