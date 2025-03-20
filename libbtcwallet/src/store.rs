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

use bitcoin_payment_instructions::amount::Amount;

use ldk_node::bitcoin::hex::{DisplayHex, FromHex};

use ldk_node::lightning::types::payment::PaymentPreimage;
use ldk_node::lightning::util::persist::KVStore;
use ldk_node::lightning::util::ser::{Readable, Writeable};
use ldk_node::lightning::{impl_writeable_tlv_based, impl_writeable_tlv_based_enum};

use crate::custodial_wallet::CustodialPaymentId;

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::str::FromStr;

const STORE_PRIMARY_KEY: &'static str = "libbtcwallet";
const STORE_SECONDARY_KEY: &'static str = "payment_store";

#[derive(Clone, Copy, Debug)]
pub enum TxStatus {
	Pending,
	Completed,
	Failed,
}

#[derive(Clone, Debug)]
pub struct Transaction {
	pub status: TxStatus,
	pub outbound: bool,
	pub amount: Amount,
	pub fee: Amount,
	pub payment_type: PaymentType,
}

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub(crate) enum PaymentId {
	Lightning([u8; 32]),
	Custodial(CustodialPaymentId),
}
impl fmt::Display for PaymentId {
	fn fmt(&self, fmt: &mut fmt::Formatter) -> Result<(), fmt::Error> {
		match self {
			PaymentId::Lightning(bytes) => write!(fmt, "LN-{}", (&bytes).as_hex()),
			PaymentId::Custodial(s) => write!(fmt, "CU-{}", s),
		}
	}
}
impl FromStr for PaymentId {
	type Err = ();
	fn from_str(s: &str) -> Result<PaymentId, ()> {
		if s.len() < 4 { return Err(()); }
		match &s[..3] {
			"LN-" => {
				let id = FromHex::from_hex(&s[3..]).map_err(|_| ())?;
				Ok(PaymentId::Lightning(id))
			},
			"CU-" => Ok(PaymentId::Custodial(s[3..].to_owned())),
			_ => Err(())
		}
	}
}
impl_writeable_tlv_based_enum!(PaymentId,
	{0, Lightning} => (),
	{1, Custodial} => (),
);

#[derive(Clone, Debug)]
pub enum PaymentType {
	OutgoingLightningBolt12 {
		/// The lightning "payment preimage" which represents proof that the payment completed.
		/// Will be set for any [`TxStatus::Completed`] payments.
		payment_preimage: Option<PaymentPreimage>,
		//offer: Offer,
		// TODO PoP
	},
	OutgoingLightningBolt11 {
		/// The lightning "payment preimage" which represents proof that the payment completed.
		/// Will be set for any [`TxStatus::Completed`] payments.
		payment_preimage: Option<PaymentPreimage>,
		//invoice: Bolt11Invoice,
	},
	OutgoingOnChain {
		// TODO txid
	},
	IncomingOnChain {
		// TODO txid
	},
	IncomingLightning {
		// TODO: Give all payment instructions an id so that incoming can get matched
	},
}

impl_writeable_tlv_based_enum!(PaymentType,
	(0, OutgoingLightningBolt12) => { (0, payment_preimage, option), },
	(1, OutgoingLightningBolt11) => { (0, payment_preimage, option), },
	(2, OutgoingOnChain) => { },
	(3, IncomingOnChain) => { },
	(4, IncomingLightning) => { },

);

#[derive(Debug)]
pub(crate) enum TxType {
	TransferToNonCustodial {
		custodial_payment: CustodialPaymentId,
		lightning_payment: [u8; 32],
		payment_triggering_transfer: PaymentId,
	},
	PaymentTriggeringTransferToNonCustodial {
		ty: PaymentType,
	},
	Payment {
		ty: PaymentType,
	},
}
impl_writeable_tlv_based_enum!(TxType,
	(0, TransferToNonCustodial) => {
		(0, custodial_payment, required),
		(2, lightning_payment, required),
		(4, payment_triggering_transfer, required),
	},
	(1, PaymentTriggeringTransferToNonCustodial) => { (0, ty, required), },
	(2, Payment) => { (0, ty, required), },
);

#[derive(Debug)]
pub(crate) struct TxMetadata {
	// TODO: TIME
	pub(crate) ty: TxType,
}
impl_writeable_tlv_based!(TxMetadata, { (0, ty, required) });

pub(crate) struct TxMetadataStore {
	tx_metadata: RwLock<HashMap<PaymentId, TxMetadata>>,
	store: Arc<dyn KVStore + Send + Sync>,
}

impl TxMetadataStore {
	pub fn new(store: Arc<dyn KVStore + Send + Sync>) -> TxMetadataStore {
		let keys = store.list(STORE_PRIMARY_KEY, STORE_SECONDARY_KEY)
			.expect("We do not allow reads to fail");
		let mut tx_metadata = HashMap::new();
		for key in keys {
			let data_bytes = store.read(STORE_PRIMARY_KEY, STORE_SECONDARY_KEY, &key)
				.expect("We do not allow reads to fail");
			let key = PaymentId::from_str(&key).expect("Invalid key in transaction metadata storage");
			let data = Readable::read(&mut &data_bytes[..]).expect("Invalid data in transaction metadata storage");
			tx_metadata.insert(key, data);
		}
		// TODO: Read
		TxMetadataStore {
			store,
			tx_metadata: RwLock::new(tx_metadata),
		}
	}

	pub fn read<'a>(&'a self) -> RwLockReadGuard<'a, HashMap<PaymentId, TxMetadata>> {
		self.tx_metadata.read().unwrap()
	}

	pub fn insert(&self, key: PaymentId, value: TxMetadata) {
		let mut tx_metadata = self.tx_metadata.write().unwrap();
		let key_str = key.to_string();
		let ser = value.encode();
		let old = tx_metadata.insert(key, value);
		debug_assert!(old.is_none());
		self.store.write(STORE_PRIMARY_KEY, STORE_SECONDARY_KEY, &key_str, &ser)
			.expect("We do not allow writes to fail");
	}
}
