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

		//let lsp = ("0264a62a4307d701c04a46994ce5f5323b1ca28c80c66b73c631dbcb0990d6e835", "46.105.57.11:9735");
		//let lsp = ("0288be11d147e1525f7f234f304b094d6627d2c70f3313d7ba3696887b261c4447", "18.219.93.203:9735");
		//let lsp = ("02a7311555b28f6851698999c8045de5dd3a355feae444e6ca1300bb52cceb1e7b", "88.99.184.172:9735");
		//let lsp = ("02cad4ec4c8b0dc2c7035a1898f979c8c7169bdff74e01ad6ca7aea59d85c59e8b", "159.69.32.62:20757");
		let lsp = ("0370a5392cd7c81ff5128fa656ee6db0c4d11c778fcd6cb98cb6ba3b48394f5705", "192.243.215.101:26000");

		let config = WalletConfig {
			storage_config: StorageConfig::LocalSQLite("/home/matt/demo-wallet/".to_owned()),
			chain_source: ChainSource::Esplora("https://blockstream.info/testnet/api".to_string()),
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
