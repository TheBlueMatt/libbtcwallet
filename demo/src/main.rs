use tokio::runtime::Runtime;
use std::sync::Arc;

use bitcoin_payment_instructions::amount::Amount;

use libbtcwallet::{ChainSource, StorageConfig, PaymentInfo, Wallet, WalletConfig};

use ldk_node::bitcoin::Network;
use ldk_node::bitcoin::hex::FromHex;

fn main() {
	let runtime = Arc::new(Runtime::new().unwrap());
	let runtime_ref = Arc::clone(&runtime);
	runtime.block_on(async move {
		let mut seed = [0; 64];
		seed.copy_from_slice(&Vec::<u8>::from_hex("PUT 64 bytes here").unwrap()[..]);

		let lsp = ("021deaa26ce6bb7cc63bd30e83a2bba1c0368269fa3bb9b616a24f40d941ac7d32", "69.59.18.144:9735");

		let config = WalletConfig {
			storage_config: StorageConfig::LocalSQLite("/home/matt/demo-wallet/".to_owned()),
			chain_source: ChainSource::Esplora("https://blockstream.info/api".to_string()),
			lsp: (lsp.1.parse().unwrap(), lsp.0.parse().unwrap(), None),
			network: Network::Bitcoin,
			seed,
			tunables: Default::default(),
		};
		let wallet = Wallet::new(runtime_ref, config).await.unwrap();
		eprintln!("BUILT WALLET!");
		eprintln!("Balances: {:?}", wallet.get_balance().await);
		//eprintln!("Txn: {:?}", wallet.list_transactions().await.unwrap());
		//let hundred_sats_uri = wallet.get_single_use_receive_uri(Some(Amount::from_sats(100))).await.unwrap();
		//eprintln!("Deposit instructions for 100 sats: {}", hundred_sats_uri);
		//let hundred_sats_uri = wallet.get_single_use_receive_uri(Some(Amount::from_sats(10_000))).await.unwrap();
		//eprintln!("Deposit instructions for 10_000 sats: {}", hundred_sats_uri);
		//let million_sats_uri = wallet.get_single_use_receive_uri(Some(Amount::from_sats(150_000))).await.unwrap();
		//eprintln!("Deposit instructions for 150_000 sats: {}", million_sats_uri);

		let mut sin = std::io::stdin().lines();
		while let Some(Ok(line)) = sin.next() {
			let mut words = line.split(' ');
			match words.next().unwrap_or("") {
				"deposit" => {
					if let Ok(amt) = words.next().unwrap_or("").parse() {
						let uri = wallet.get_single_use_receive_uri(Some(Amount::from_sats(amt))).await;
						eprintln!("{:?}", uri);
					} else {
						println!("Bad amount");
					}
				},
				"parse" => {
					let res = wallet.parse_payment_instructions(words.next().unwrap_or("")).await;
					eprintln!("{:?}", res);
				}
				"send" => {
					let instructions = words.next().unwrap_or("");
					let res = wallet.parse_payment_instructions(instructions).await;
					if let Ok(res) = res {
						if let Ok(amt) = words.next().unwrap_or("").parse() {
							let info = PaymentInfo::set_amount(res, Amount::from_milli_sats(amt));
							if let Ok(info) = info {
								let res = wallet.pay(&info).await;
								eprintln!("{:?}", res);
							} else {
								println!("amount didn't match instructions");
							}
						} else {
							println!("Bad amount");
						}
					} else {
						println!("Bad payment instructions");
					}
				},
				"transactions"|"txn" => {
					println!("{:?}", wallet.list_transactions().await.unwrap());
				},
				"balance"|"bal" => {
					println!("{:?}", wallet.get_balance().await);
				},
				_ => {
					println!("Unknown instruction");
				}
			}
		}
	});

}
