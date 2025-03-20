use crate::{InitFailure, WalletConfig, TxStatus};

use ldk_node::bitcoin::hashes::sha256::Hash as Sha256;
use ldk_node::bitcoin::hashes::Hash;
use ldk_node::bitcoin::Network;

use bitcoin_payment_instructions::PaymentMethod;
use bitcoin_payment_instructions::amount::Amount;

use lightning_invoice::Bolt11Invoice; // TODO: Re-export this from `lightning`

use spark_rust::{SparkSdk, SparkNetwork};
use spark_rust::error::SparkSdkError;
use spark_rust::signer::default_signer::DefaultSigner;
use spark_rust::signer::traits::SparkSigner;

use spark_protos::spark::TransferStatus;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

pub(crate) type CustodialPaymentId = String;

pub(crate) type Error = SparkSdkError;

pub(crate) struct Payment {
	pub(crate) id: CustodialPaymentId,
	pub(crate) amount: Amount,
	pub(crate) fee: Amount,
	pub(crate) status: TxStatus,
}

impl From<TransferStatus> for TxStatus {
	fn from(o: TransferStatus) -> TxStatus {
		match o {
			TransferStatus::SenderInitiated|TransferStatus::SenderKeyTweakPending|TransferStatus::SenderKeyTweaked|TransferStatus::ReceiverKeyTweaked =>
				TxStatus::Pending,
			TransferStatus::Completed => TxStatus::Completed,
			TransferStatus::Expired|TransferStatus::Returned|TransferStatus::TransferStatusrReceiverRefundSigned => TxStatus::Failed,
		}
	}
}

pub(crate) trait CustodialWalletInterface: Sized {
	fn init(config: &WalletConfig) -> impl Future<Output = Result<Self, InitFailure>> + Send;
	fn get_balance(&self) -> Amount;
	fn get_reusable_receive_uri(&self) -> impl Future<Output = Result<String, Error>> + Send;
	fn get_bolt11_invoice(&self, amount: Option<Amount>) -> impl Future<Output = Result<Bolt11Invoice, Error>> + Send;
	fn list_payments(&self) -> impl Future<Output = Result<Vec<Payment>, Error>> + Send;
	fn estimate_fee(&self, method: &PaymentMethod, amount: Amount) -> impl Future<Output = Result<Amount, Error>> + Send;
	fn pay(&self, method: &PaymentMethod, amount: Amount) -> impl Future<Output = Result<CustodialPaymentId, Error>> + Send;
}

pub(crate) struct SparkWallet {
	spark_wallet: Arc<SparkSdk>,
}

impl CustodialWalletInterface for SparkWallet {
	fn init(config: &WalletConfig) -> impl Future<Output = Result<Self, InitFailure>> + Send {
		async move {
			let seed = Sha256::hash(&config.seed);
			let net = match config.network {
				Network::Bitcoin => SparkNetwork::Mainnet,
				Network::Regtest => SparkNetwork::Regtest,
				_ => Err(Error::General("Unsupported network".to_owned()))?,
			};
			let signer = DefaultSigner::from_master_seed(&seed[..], net).await?;
			let spark_wallet = Arc::new(SparkSdk::new(net, signer).await?);

			let spark_ref = Arc::clone(&spark_wallet);
			tokio::spawn(async move {
				loop {
					let _ = spark_ref.sync_wallet().await;
					tokio::time::sleep(Duration::from_secs(30)).await;
				}
			});

			Ok(SparkWallet { spark_wallet })
		}
	}
	fn get_balance(&self) -> Amount {
		Amount::from_sats(self.spark_wallet.get_bitcoin_balance())
	}
	fn get_reusable_receive_uri(&self) -> impl Future<Output = Result<String, Error>> + Send {
		async move {
			todo!()
		}
	}
	fn get_bolt11_invoice(&self, amount: Option<Amount>) -> impl Future<Output = Result<Bolt11Invoice, Error>> + Send {
		async move {
			// TODO: get upstream to let us be amount-less
			let inv = self.spark_wallet.create_lightning_invoice(amount.unwrap_or(Amount::from_sats(0)).sats_rounding_up(), None, None).await?;
			// TODO: Drop once ldk-node upgrades to ldk 0.1
			use std::str::FromStr;
			Ok(Bolt11Invoice::from_str(&inv.to_string()).unwrap())
		}
	}
	fn list_payments(&self) -> impl Future<Output = Result<Vec<Payment>, Error>> + Send {
		async move {
			let transfers = self.spark_wallet.get_all_transfers(None, None).await?.transfers;
			let mut res = Vec::with_capacity(transfers.len());
			for transfer in transfers {
				res.push(Payment {
					status: transfer.status().into(),
					id: transfer.id,
					amount: Amount::from_sats(transfer.total_value),
					fee: Amount::from_sats(0), // TODO: Missing upstream?
				});
			}
			Ok(res)
		}
	}
	fn estimate_fee(&self, method: &PaymentMethod, amount: Amount) -> impl Future<Output = Result<Amount, Error>> + Send {
		async move {
			if let PaymentMethod::LightningBolt11(invoice) = method {
				self.spark_wallet.get_lightning_send_fee_estimate(invoice.to_string()).await
					.map(|fees| Amount::from_sats(fees.fees))
			} else {
				Err(Error::General("Only BOLT 11 is currently supported".to_owned()))
			}
		}
	}
	fn pay(&self, method: &PaymentMethod, amount: Amount) -> impl Future<Output = Result<CustodialPaymentId, Error>> + Send {
		async move {
			if let PaymentMethod::LightningBolt11(invoice) = method {
				self.spark_wallet.pay_lightning_invoice(&invoice.to_string()).await
			} else {
				Err(Error::General("Only BOLT 11 is currently supported".to_owned()))
			}
		}
	}
}
