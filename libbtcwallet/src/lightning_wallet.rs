use crate::{ChainSource, InitFailure, WalletConfig, TxStatus, PaymentType};

use bitcoin_payment_instructions::PaymentMethod;
use bitcoin_payment_instructions::amount::Amount;

use ldk_node::{Event, NodeError};
use ldk_node::bitcoin::{Address, Network};
use ldk_node::payment::{PaymentDetails, PaymentDirection, PaymentStatus, PaymentKind};
use ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_node::lightning::util::persist::KVStore;

use lightning_invoice::Bolt11Invoice; // TODO: Re-export this from `lightning`

use std::sync::Arc;

use tokio::runtime::Runtime;

impl From<PaymentStatus> for TxStatus {
	fn from(o: PaymentStatus) -> TxStatus {
		match o {
			PaymentStatus::Pending => TxStatus::Pending,
			PaymentStatus::Succeeded => TxStatus::Completed,
			PaymentStatus::Failed => TxStatus::Failed,
		}
	}
}

impl From<&PaymentDetails> for PaymentType {
	fn from(d: &PaymentDetails) -> PaymentType {
		match (&d.kind, d.direction == PaymentDirection::Outbound) {
			(PaymentKind::Bolt11 { preimage, .. }|PaymentKind::Bolt11Jit { preimage, .. }, true) => {
				if d.status == PaymentStatus::Succeeded {
					debug_assert!(preimage.is_some());
				}
				PaymentType::OutgoingLightningBolt11 {
					payment_preimage: *preimage,
				}
			},
			(PaymentKind::Bolt12Offer { preimage, .. }, true) => {
				PaymentType::OutgoingLightningBolt12 {
					payment_preimage: *preimage,
				}
			},
			(PaymentKind::Bolt12Refund { preimage, .. }|PaymentKind::Spontaneous { preimage, .. }, true) => {
				debug_assert!(false);
				PaymentType::OutgoingLightningBolt12 {
					payment_preimage: *preimage,
				}
			},
			(PaymentKind::Onchain, true) => {
				PaymentType::OutgoingOnChain {}
			}
			(PaymentKind::Onchain, false) => {
				PaymentType::IncomingOnChain {}
			},
			(_, false) => {
				PaymentType::IncomingLightning {}
			},
		}
	}
}

pub(crate) struct LightningWallet {
	ldk_node: Arc<ldk_node::Node>,
}

impl LightningWallet {
	pub(super) fn init(runtime: Arc<Runtime>, config: WalletConfig, store: Arc<dyn KVStore + Sync + Send>) -> Result<Self, InitFailure> {
		let mut builder = ldk_node::Builder::new();
		builder.set_network(config.network);
		builder.set_entropy_seed_bytes(config.seed.to_vec())?;
		if config.network == Network::Testnet {
			// TODO: For now!
			builder.set_gossip_source_rgs("https://rapidsync.lightningdevkit.org/testnet/snapshot".to_string());
		} else {
			builder.set_gossip_source_rgs("https://rapidsync.lightningdevkit.org/snapshot".to_string());
		}
		builder.set_liquidity_source_lsps2(config.lsp.0, config.lsp.1, config.lsp.2);
		match config.chain_source {
			ChainSource::Esplora(url) => builder.set_chain_source_esplora(url, None),
			ChainSource::BitcoindRPC { host, port, user, password } =>
				builder.set_chain_source_bitcoind_rpc(host, port, user, password),
		};

		let ldk_node = Arc::new(builder.build_with_store(store)?);

		ldk_node.start_with_runtime(Arc::clone(&runtime))?;

		let events_ldk_node = Arc::clone(&ldk_node);
		runtime.spawn(async move {
			loop {
				match events_ldk_node.next_event_async().await {
					Event::PaymentSuccessful { .. } => {},
					Event::PaymentFailed { .. } => {},
					Event::PaymentReceived { .. } => {},
					Event::PaymentClaimable { .. } => {},
					Event::ChannelPending { .. } => {},
					Event::ChannelReady { .. } => {},
					Event::ChannelClosed { .. } => {
						// TODO: Oof! Probably open a new channel with our LSP
					},
				}
			}
		});

		Ok(Self { ldk_node })
	}

	pub(crate) fn get_on_chain_address(&self) -> Result<Address, NodeError> {
		self.ldk_node.onchain_payment().new_address()
	}

	pub(crate) async fn get_bolt11_invoice(&self, amount: Option<Amount>) -> Result<Bolt11Invoice, NodeError> {
		// TODO: `receive_via_jit_channel` should not use the jit channel if there's enough balance
		// on the non-JIT channel, but we should check that (in the spec/impl?)
		let inv_str = if let Some(amt) = amount {
			self.ldk_node.bolt11_payment().receive_via_jit_channel(amt.msats(), "", 86400, None)
		} else {
			self.ldk_node.bolt11_payment().receive_variable_amount_via_jit_channel("", 86400, None)
		}?.to_string();
		// TODO: Drop this once ldk-node upgrades to 0.1
		use std::str::FromStr;
		let inv = Bolt11Invoice::from_str(&inv_str).unwrap();
		Ok(inv)
	}

	pub(crate) fn list_payments(&self) -> Vec<PaymentDetails> {
		self.ldk_node.list_payments()
	}

	pub(crate) fn get_balance(&self) -> (Amount, Amount) {
		let balances = self.ldk_node.list_balances();
		(
			Amount::from_sats(balances.total_lightning_balance_sats),
			Amount::from_sats(balances.total_onchain_balance_sats),
		)
	}

	pub(crate) fn estimate_receivable_balance(&self) -> Amount {
		// Estimate the amount we can receive. We currently just try to figure out how much we can
		// receive in a single channel. While this could be an underestimate, hopefully by the time
		// any of this ships we support splicing and no one ever ends up with more than one channel
		// anyway.
		if let Some(chan) = self.ldk_node.list_channels().first() {
			return Amount::from_milli_sats(chan.inbound_capacity_msat);
		}
		Amount::from_sats(0)
	}

	pub(crate) async fn estimate_fee(&self, method: &PaymentMethod, amount: Amount) -> Result<Amount, NodeError> {
		// TODO: Implement this in ldk-node!
		Ok(Amount::from_sats(0))
	}

	pub(crate) async fn pay(&self, method: &PaymentMethod, amount: Amount) -> Result<PaymentId, NodeError> {
		use std::str::FromStr; // TODO: Drop this once ldk-node upgrades to 0.1
		match method {
			PaymentMethod::LightningBolt11(invoice) => {
				// TODO: Drop this once ldk-node upgrades to 0.1
				let inv_str = invoice.to_string();
				let inv = ldk_node::lightning_invoice::Bolt11Invoice::from_str(&inv_str).unwrap();
				self.ldk_node.bolt11_payment().send_using_amount(&inv, amount.msats(), None)
			},
			PaymentMethod::LightningBolt12(offer) => {
				// TODO: Drop this once ldk-node upgrades to 0.1
				let offer_str = offer.to_string();
				let off = ldk_node::lightning::offers::offer::Offer::from_str(&offer_str).unwrap();
				self.ldk_node.bolt12_payment().send_using_amount(&off, amount.msats(), None, None)
			},
			PaymentMethod::OnChain { address, amount: _ } => {
				self.ldk_node.onchain_payment().send_to_address(address, amount.sats_rounding_up())
					.map(|txid| PaymentId(*txid.as_ref()))
			},
		}
	}

	pub(crate) fn stop(&self) {
		let _ = self.ldk_node.stop();
	}
}
