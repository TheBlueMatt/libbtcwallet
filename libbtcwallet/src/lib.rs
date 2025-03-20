//! A library implementing the full backend for a modern, highly usable, Bitcoin wallet focusing on
//! maximizing security and self-custody without trading off user experience.
//!
//! This crate should do everything you need to build a great Bitcoin wallet, except the UI.
//!
//! In order to maximize the user experience, small balances are held in a custodial service (XXX
//! which one), avoiding expensive setup fees, while larger balances are moved into on-chain
//! lightning channels, ensuring trust is minimized in the custodial service.
//!
//! Despite funds being stored in multiple places, the full balance can be treated as a single
//! wallet - payments can draw on both balances simultaneously and deposits are automatically
//! shifted to minimize fees and ensure maximal security.

use bitcoin_payment_instructions as instructions;
use bitcoin_payment_instructions::{PaymentInstructions, http_resolver::HTTPHrnResolver};

pub use bitcoin_payment_instructions::PaymentMethod;
use bitcoin_payment_instructions::amount::Amount;

use ldk_node::bitcoin::Network;
use ldk_node::bitcoin::io;
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::{BuildError, NodeError};
use ldk_node::payment::PaymentKind as LightningPaymentKind;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning::util::persist::KVStore;
use ldk_node::io::sqlite_store::SqliteStore;

use tokio::runtime::Runtime;

use std::cmp;
use std::collections::HashMap;
use std::fmt::Write;
use std::time::Duration;
use std::sync::Arc;

mod custodial_wallet;
mod lightning_wallet;
mod store;

use lightning_wallet::LightningWallet;
use custodial_wallet::CustodialWalletInterface;
use custodial_wallet::Error as CustodialError;

type CustodialWallet = custodial_wallet::SparkWallet;

pub use store::{TxStatus, Transaction, PaymentType};
use store::{PaymentId, TxType, TxMetadata, TxMetadataStore};

#[derive(Debug)]
pub struct Balances {
	available_balance: Amount,
	pending_balance: Amount,
}

struct WalletImpl {
	ln_wallet: LightningWallet,
	custodial: CustodialWallet,
	tunables: Tunables,
	network: Network,
	tx_metadata: TxMetadataStore,
	store: Arc<dyn KVStore + Send + Sync>,
}

pub struct Wallet {
	inner: Arc<WalletImpl>,
}

pub enum VssAuth {
	LNURLAuthServer(String),
	FixedHeaders(HashMap<String, String>),
}

pub struct VssConfig {
	vss_url: String,
	store_id: String,
	headers: VssAuth,
}

pub enum StorageConfig {
	LocalSQLite(String),
	//VSS(VssConfig),
}

pub enum ChainSource {
	//Electrum(String),
	Esplora(String),
	BitcoindRPC {
		host: String,
		port: u16,
		user: String,
		password: String,
	},
}

pub struct WalletConfig {
	pub storage_config: StorageConfig,
	pub chain_source: ChainSource,
	pub lsp: (SocketAddress, PublicKey, Option<String>),
	pub network: Network,
	pub seed: [u8; 64],
	pub tunables: Tunables,
}

#[derive(Clone, Copy)]
pub struct Tunables {
	pub custodial_balance_limit: Amount,
	/// Custodial balances below this threshold will not be transferred to non-custodial balance
	/// even if we have capacity to do so without paying for a new channel.
	///
	/// This avoids unecessary transfers and fees.
	pub rebalance_min: Amount,
	/// Payment instructions generated using [`Wallet::get_single_use_receive_uri`] for an amount
	/// below this threshold will not include an on-chain address.
	pub onchain_receive_threshold: Amount,
	/// Payment instructions generated using [`Wallet::get_single_use_receive_uri`] with no amount
	/// will only include an on-chain address if this is set.
	pub enable_amountless_receive_on_chain: bool,
}

impl Default for Tunables {
	fn default() -> Self {
		Tunables {
			custodial_balance_limit: Amount::from_sats(100_000),
			rebalance_min: Amount::from_sats(5_000),
			onchain_receive_threshold: Amount::from_sats(10_000),
			enable_amountless_receive_on_chain: true,
		}
	}
}

/// A payable version of [`PaymentInstructions`] (i.e. with a set amount).
pub struct PaymentInfo((PaymentInstructions, Amount));

impl PaymentInfo {
	/// Prepares us to pay a [`PaymentInstructions`] by setting the amount.
	///
	/// If [`PaymentInstructions`] already contains an `amount`, the `amount` must match either
	/// [`PaymentInstructions::ln_payment_amount`] or
	/// [`PaymentInstructions::onchain_payment_amount`] exactly. Will return `Err(())` if this
	/// requirement is not met.
	///
	/// Otherwise, this amount can be any value.
	pub fn set_amount(instructions: PaymentInstructions, amount: Amount) -> Result<PaymentInfo, ()> {
		let ln_amt = instructions.ln_payment_amount();
		let onchain_amt = instructions.onchain_payment_amount();
		let ln_amt_matches = ln_amt.is_some() && ln_amt.unwrap() == amount;
		let onchain_amt_matches = onchain_amt.is_some() && onchain_amt.unwrap() == amount;

		if (ln_amt.is_some() || onchain_amt.is_some()) && !ln_amt_matches && !onchain_amt_matches {
			Err(())
		} else {
			Ok(PaymentInfo((instructions, amount)))
		}
	}
}

#[derive(Debug)]
pub enum InitFailure {
	IoError(io::Error),
	LdkNodeBuildFailure(BuildError),
	LdkNodeStartFailure(NodeError),
	CustodialFailure(CustodialError),
}
impl From<io::Error> for InitFailure {
	fn from(e: io::Error) -> InitFailure {
		InitFailure::IoError(e)
	}
}
impl From<BuildError> for InitFailure {
	fn from(e: BuildError) -> InitFailure {
		InitFailure::LdkNodeBuildFailure(e)
	}
}
impl From<NodeError> for InitFailure {
	fn from(e: NodeError) -> InitFailure {
		InitFailure::LdkNodeStartFailure(e)
	}
}
impl From<CustodialError> for InitFailure {
	fn from(e: CustodialError) -> InitFailure {
		InitFailure::CustodialFailure(e)
	}
}

#[derive(Debug)]
pub enum WalletError {
	LdkNodeFailure(NodeError),
	CustodialFailure(CustodialError),
}
impl From<CustodialError> for WalletError {
	fn from(e: CustodialError) -> WalletError {
		WalletError::CustodialFailure(e)
	}
}
impl From<NodeError> for WalletError {
	fn from(e: NodeError) -> WalletError {
		WalletError::LdkNodeFailure(e)
	}
}

impl Wallet {
	/// Constructs a new Wallet.
	///
	/// `runtime` must be a reference to the running `tokio` runtime which we are currently
	/// operating in.
	// TODO: WOW that is a terrible API lol
	pub async fn new(runtime: Arc<Runtime>, config: WalletConfig) -> Result<Wallet, InitFailure> {
		let tunables = config.tunables;
		let network = config.network;
		let store: Arc<dyn KVStore + Send + Sync> = match &config.storage_config {
			StorageConfig::LocalSQLite(path) => {
				Arc::new(SqliteStore::new(path.into(), Some("libbtcwallet.sqlite".to_owned()), None)?)
			},
		};
		let custodial = CustodialWallet::init(&config).await?;
		let ln_wallet = LightningWallet::init(Arc::clone(&runtime), config, Arc::clone(&store))?;

		let inner = Arc::new(WalletImpl {
			custodial,
			ln_wallet,
			network,
			tunables,
			tx_metadata: TxMetadataStore::new(Arc::clone(&store)),
			store,
		});

		// TODO: events from ldk-node up
		Ok(Wallet { inner })
	}

	fn init_custodial_rebalance(&self, triggering_transaction_id: PaymentId) {
		let inner = Arc::clone(&self.inner);
		tokio::spawn(async move {
			let lightning_receivable = inner.ln_wallet.estimate_receivable_balance();
			let custodial_balance = inner.custodial.get_balance();
			let transfer_amt = cmp::min(lightning_receivable, custodial_balance);
			if transfer_amt > inner.tunables.rebalance_min {
				if let Ok(inv) = inner.ln_wallet.get_bolt11_invoice(Some(transfer_amt)).await {
					//TODO: Drop this once ldk-node upgrades to 0.1
					let inv_str = inv.to_string();
					use std::str::FromStr;
					let inv = lightning_invoice::Bolt11Invoice::from_str(&inv_str).unwrap();
					let expected_hash = *inv.payment_hash();
					if let Ok(rebalance_id) = inner.custodial.pay(&PaymentMethod::LightningBolt11(inv), transfer_amt).await {
						let mut received_payment_id = None;
						for payment in inner.ln_wallet.list_payments() {
							if let LightningPaymentKind::Bolt11 { hash, .. } = payment.kind {
								if &hash.0[..] == &expected_hash[..] {
									received_payment_id = Some(payment.id);
									break;
								}
							}
						}
						debug_assert!(received_payment_id.is_some());
						inner.tx_metadata.insert(PaymentId::Custodial(rebalance_id.clone()), TxMetadata {
							ty: TxType::TransferToNonCustodial {
								custodial_payment: rebalance_id,
								lightning_payment: received_payment_id.map(|id| id.0).unwrap_or([0; 32]),
								payment_triggering_transfer: triggering_transaction_id,
							},
						});
					}
				}
			}
		});
	}

	/// Lists the transactions which have been made.
	pub async fn list_transactions(&self) -> Result<Vec<Transaction>, WalletError> {
		let custodial_payments = self.inner.custodial.list_payments().await?;
		let lightning_payments = self.inner.ln_wallet.list_payments();

		let mut res = Vec::with_capacity(custodial_payments.len() + lightning_payments.len());
		let tx_metadata = self.inner.tx_metadata.read();

		let mut internal_transfers = HashMap::new();
		struct InternalTransfer {
			lightning_receive_fee: Option<Amount>,
			custodial_send_fee: Option<Amount>,
			transaction: Option<Transaction>,
		}

		for payment in custodial_payments {
			if let Some(tx_metadata) = tx_metadata.get(&PaymentId::Custodial(payment.id.clone())) {
				match &tx_metadata.ty {
					TxType::TransferToNonCustodial { custodial_payment, lightning_payment, payment_triggering_transfer } => {
						let entry = internal_transfers.entry((*payment_triggering_transfer).clone())
							.or_insert(InternalTransfer {
								lightning_receive_fee: None,
								custodial_send_fee: None,
								transaction: None,
							});
						if payment.id == *custodial_payment {
							debug_assert!(entry.custodial_send_fee.is_none());
							entry.custodial_send_fee = Some(payment.fee);
						} else {
							debug_assert!(false);
						}
					},
					TxType::PaymentTriggeringTransferToNonCustodial { ty } => {
						let entry = internal_transfers.entry(PaymentId::Custodial(payment.id.clone()))
							.or_insert(InternalTransfer {
								lightning_receive_fee: None,
								custodial_send_fee: None,
								transaction: None,
							});
						debug_assert!(entry.transaction.is_none());
						entry.transaction = Some(Transaction {
							status: payment.status,
							outbound: false, // XXX: debug_assert this from spark
							amount: payment.amount,
							fee: payment.fee,
							payment_type: ty.clone(),
						});
					},
					TxType::Payment { ty } => {
						debug_assert!(!matches!(ty, PaymentType::OutgoingOnChain { .. }));
						debug_assert!(!matches!(ty, PaymentType::IncomingOnChain { .. }));
						res.push(Transaction {
							status: payment.status,
							outbound: true, // XXX: need this from spark
							amount: payment.amount,
							fee: payment.fee,
							payment_type: ty.clone(),
						});
					},
				}
			} else {
eprintln!("txn list {:?}", tx_metadata);
eprintln!("tx id {}", payment.id);
				debug_assert!(false);
			}
		}
		for payment in lightning_payments {
			if let Some(tx_metadata) = tx_metadata.get(&PaymentId::Lightning(payment.id.0)) {
				match &tx_metadata.ty {
					TxType::TransferToNonCustodial { custodial_payment, lightning_payment, payment_triggering_transfer } => {
						let entry = internal_transfers.entry(payment_triggering_transfer.clone())
							.or_insert(InternalTransfer {
								lightning_receive_fee: None,
								custodial_send_fee: None,
								transaction: None,
							});
						if payment.id.0 == *lightning_payment {
							debug_assert!(entry.lightning_receive_fee.is_none());
							entry.lightning_receive_fee = Some(Amount::from_milli_sats(0)); // TODO: https://github.com/lightningdevkit/ldk-node/issues/494
						} else {
							debug_assert!(false);
						}
					},
					TxType::PaymentTriggeringTransferToNonCustodial { ty } => {
						let entry = internal_transfers.entry(PaymentId::Lightning(payment.id.0))
							.or_insert(InternalTransfer {
								lightning_receive_fee: None,
								custodial_send_fee: None,
								transaction: None,
							});
						debug_assert!(entry.transaction.is_none());
						debug_assert!(payment.direction == ldk_node::payment::PaymentDirection::Outbound);
						entry.transaction = Some(Transaction {
							status: payment.status.into(),
							outbound: payment.direction == ldk_node::payment::PaymentDirection::Outbound,
							amount: Amount::from_milli_sats(payment.amount_msat.unwrap_or(0)), // TODO: when can this be none https://github.com/lightningdevkit/ldk-node/issues/495
							fee: Amount::from_milli_sats(0), // TODO: https://github.com/lightningdevkit/ldk-node/issues/494
							payment_type: (&payment).into(),
						});
					},
					TxType::Payment { ty } => {
						debug_assert!(false); // Shouldn't even have an entry
						res.push(Transaction {
							status: payment.status.into(),
							outbound: payment.direction == ldk_node::payment::PaymentDirection::Outbound,
							amount: Amount::from_milli_sats(payment.amount_msat.unwrap_or(0)), // TODO: when can this be none https://github.com/lightningdevkit/ldk-node/issues/495
							fee: Amount::from_milli_sats(0), // TODO: https://github.com/lightningdevkit/ldk-node/issues/494
							payment_type: (&payment).into(),
						})
					},
				}
			} else {
				res.push(Transaction {
					status: payment.status.into(),
					outbound: payment.direction == ldk_node::payment::PaymentDirection::Outbound,
					amount: Amount::from_milli_sats(payment.amount_msat.unwrap_or(0)), // TODO: when can this be none https://github.com/lightningdevkit/ldk-node/issues/495
					fee: Amount::from_milli_sats(0), // TODO: https://github.com/lightningdevkit/ldk-node/issues/494
					payment_type: (&payment).into(),
				})
			}
		}

		for (_, tx_info) in internal_transfers {
			debug_assert!(tx_info.lightning_receive_fee.is_some());
			debug_assert!(tx_info.custodial_send_fee.is_some());
			debug_assert!(tx_info.transaction.is_some());
			if let Some(mut transaction) = tx_info.transaction {
				transaction.fee = transaction.fee.saturating_add(tx_info.lightning_receive_fee.unwrap_or(Amount::from_sats(0)));
				transaction.fee = transaction.fee.saturating_add(tx_info.custodial_send_fee.unwrap_or(Amount::from_sats(0)));
				res.push(transaction);
			}
		}

		Ok(res)
	}

	/// Gets our current total balance
	pub async fn get_balance(&self) -> Balances {
		let custodial_balance = self.inner.custodial.get_balance();
		let (available_ln, pending_balance) = self.inner.ln_wallet.get_balance();
		Balances {
			available_balance: available_ln.saturating_add(custodial_balance),
			pending_balance,
		}
	}

	/// Fetches a unique reusable BIP 321 bitcoin: URI.
	///
	/// This will generally include an on-chain address as well as a BOLT 12 lightning offer. Note
	/// that BOLT 12 offers are not universally supported across the lightning ecosystem yet, and,
	/// as such, may result in on-chain fallbacks. Use [`Self::get_single_use_receive_uri`] if
	/// possible.
	///
	/// This is suitable for inclusion in a QR code or in a BIP 353 DNS record
	pub async fn get_reusable_receive_uri(&self) -> Result<String, WalletError> {
		Ok(self.inner.custodial.get_reusable_receive_uri().await?)
	}

	/// Fetches a unique single-use BIP 321 bitcoin: URI.
	///
	/// This will generally include an on-chain address as well as a BOLT 11 lightning invoice.
	///
	/// This is suitable for inclusion in a QR code.
	pub async fn get_single_use_receive_uri(&self, amount: Option<Amount>) -> Result<String, WalletError> {
		let (enable_onchain, bolt11) = if let Some(amt) = amount {
			let enable_onchain = amt >= self.inner.tunables.onchain_receive_threshold;
			let bolt11 = if amt > self.inner.tunables.custodial_balance_limit {
				self.inner.ln_wallet.get_bolt11_invoice(amount).await?
			} else {
				self.inner.custodial.get_bolt11_invoice(amount).await?
			};
			(enable_onchain, bolt11)
		} else {
			(self.inner.tunables.enable_amountless_receive_on_chain,
				self.inner.custodial.get_bolt11_invoice(amount).await?)
		};
		let mut uri = "BITCOIN:".to_owned();
		if enable_onchain {
			write!(&mut uri, "{}", self.inner.ln_wallet.get_on_chain_address()?).unwrap();
			if let Some(amt) = amount {
				write!(&mut uri, "?AMOUNT={}&", amt.btc_decimal_rounding_up_to_sats()).unwrap();
			} else {
				write!(&mut uri, "?").unwrap();
			}
		} else {
			write!(&mut uri, "?").unwrap();
		}
		write!(&mut uri, "LIGHTNING={}", bolt11).unwrap();
		Ok(uri.to_ascii_uppercase())
	}

	/// Parses payment instructions from an arbitrary string provided by a user, scanned from a QR
	/// code, or read from a link which the user opened with this application.
	///
	/// See [`PaymentInstructions`] for the list of supported formats.
	///
	/// Note that a user might also be pasting or scanning a QR code containing a lightning BOLT 12
	/// refund, which allow us to *receive* funds, and must be parsed with
	/// [`Self::parse_claim_instructions`].
	pub async fn parse_payment_instructions(&self, instructions: &str) -> Result<PaymentInstructions, instructions::ParseError> {
		PaymentInstructions::parse_payment_instructions(instructions, self.inner.network, HTTPHrnResolver, true).await
	}

	/*/// Verifies instructions which allow us to claim funds given as:
	///  * A lightning BOLT 12 refund
	///  * An on-chain private key, which we will sweep
	// TODO: consider LNURL claim thinggy
	pub fn parse_claim_instructions(&self, instructions: &str) -> Result<..., ...> {

	}*/

	/// Estimates the fees required to pay a [`PaymentInstructions`]
	pub async fn estimate_fee(&self, _payment_info: &PaymentInstructions) -> Amount {
		todo!()
	}

	// returns true once the payment is pending
	pub async fn pay(&self, instructions: &PaymentInfo) -> Result<(), WalletError> {
		let custodial_balance = self.inner.custodial.get_balance();
		let (available_ln, _) = self.inner.ln_wallet.get_balance();

		let mut last_custodial_err = None;
		let mut last_lightning_err = None;

		let mut pay_custodial = async |method, ty: &dyn Fn() -> PaymentType| {
			if instructions.0.1 <= custodial_balance {
				if let Ok(custodial_fee) = self.inner.custodial.estimate_fee(method, instructions.0.1).await {
					if custodial_fee.saturating_add(instructions.0.1) <= custodial_balance {
						let res = self.inner.custodial.pay(method, instructions.0.1).await;
						match res {
							Ok(id) => {
								self.inner.tx_metadata.insert(PaymentId::Custodial(id), TxMetadata {
									ty: TxType::Payment { ty: ty() },
								});
								return Ok(());
							},
							Err(e) => last_custodial_err = Some(e.into()),
						}
					}
				}
			}
			Err(())
		};

		let mut pay_lightning = async |method, ty: &dyn Fn() -> PaymentType| {
			if instructions.0.1 <= available_ln {
				if let Ok(lightning_fee) = self.inner.ln_wallet.estimate_fee(method, instructions.0.1).await {
					if lightning_fee.saturating_add(instructions.0.1) <= available_ln {
						let res = self.inner.ln_wallet.pay(method, instructions.0.1).await;
						match res {
							Ok(id) => {
								self.inner.tx_metadata.insert(PaymentId::Lightning(id.0), TxMetadata {
									ty: TxType::Payment { ty: ty() },
								});
								self.init_custodial_rebalance(PaymentId::Lightning(id.0));
								return Ok(());
							},
							Err(e) => last_lightning_err = Some(e.into()),
						}
					}
				}
			}
			Err(())
		};

		// First try to pay via the custodial balance over lightning
		for method in instructions.0.0.methods() {
			match method {
				PaymentMethod::LightningBolt11(_) => {
					if pay_custodial(&method,
						&|| PaymentType::OutgoingLightningBolt11 { payment_preimage: None }
					).await.is_ok() {
						return Ok(());
					};
				},
				PaymentMethod::LightningBolt12(_) => {
					if pay_custodial(&method,
						&|| PaymentType::OutgoingLightningBolt12 { payment_preimage: None }
					).await.is_ok() {
						return Ok(());
					}
				},
				PaymentMethod::OnChain { .. } => {},
			}
		}

		// If that doesn't work, try to pay via the non-custodial balance over lightning (the
		// custodial balance can top up the lightning balance in the background)
		for method in instructions.0.0.methods() {
			match method {
				PaymentMethod::LightningBolt11(_) => {
					if pay_lightning(&method,
						&|| PaymentType::OutgoingLightningBolt11 { payment_preimage: None }
					).await.is_ok() {
						return Ok(());
					}
				},
				PaymentMethod::LightningBolt12(_) => {
					if pay_lightning(&method,
						&|| PaymentType::OutgoingLightningBolt12 { payment_preimage: None }
					).await.is_ok() {
						return Ok(());
					}
				},
				PaymentMethod::OnChain { .. } => {},
			}
		}

		//TODO: Try to MPP the payment using both custodial and LN funds

		// Finally, try custodial on-chain first,
		for method in instructions.0.0.methods() {
			match method {
				PaymentMethod::OnChain { .. } => {
					if pay_custodial(&method, &|| PaymentType::OutgoingOnChain { }).await.is_ok() {
						return Ok(());
					};
				},
				_ => {},
			}
		}

		// then pay on-chain out of the lightning wallet
		for method in instructions.0.0.methods() {
			match method {
				PaymentMethod::OnChain { .. } => {
					if pay_lightning(&method, &|| PaymentType::OutgoingOnChain { }).await.is_ok() {
						return Ok(());
					};
				},
				_ => {},
			}
		}

		Err(last_lightning_err
			.unwrap_or(last_custodial_err
				.unwrap_or(WalletError::LdkNodeFailure(NodeError::InsufficientFunds))
		))
	}
}

impl Drop for Wallet {
	fn drop(&mut self) {
		self.inner.ln_wallet.stop();
	}
}

#[cfg(test)]
mod tests {
	#[test]
	fn test_node_start() {
		// TODO
	}
}
