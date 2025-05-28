use ethers_signers::{coins_bip39::English, MnemonicBuilder};
use ethers_signers::Signer;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs};

const NUM_ACCOUNTS: usize = 200_000;
const BALANCE: &str = "0x028f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f5c28f";          // 1 000 000 ETH
const MNEMONIC: &str = "test test test test test test test test test test test junk";

#[derive(Serialize, Deserialize)]
struct AccountBalance {
    balance: String,
}

#[derive(Serialize, Deserialize)]
struct Genesis {
    nonce: String,
    timestamp: String,
    extraData: String,
    gasLimit: String,
    difficulty: String,
    mixHash: String,
    coinbase: String,
    stateRoot: String,
    number: String,
    alloc: BTreeMap<String, AccountBalance>,
    gasUsed: String,
    parentHash: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parallel HD-wallet derivation
    let alloc: BTreeMap<String, AccountBalance> = (0..NUM_ACCOUNTS)
        .into_par_iter()
        .map(|i| {
            let wallet = MnemonicBuilder::<English>::default()
                .phrase(MNEMONIC)
                .index(i as u32)               // m/44'/60'/0'/0/{i}
                .expect("bad index")            // <-- unwrap the Result
                .build()
                .expect("builder failed");      // <-- unwrap the Result

            (
                format!("{:#x}", wallet.address()),
                AccountBalance {
                    balance: BALANCE.into(),
                },
            )
        })
        .collect();

    let genesis = Genesis {
        nonce: "0x0".into(),
        timestamp: "0x6490fdd2".into(),
        extraData: "0x".into(),
        gasLimit: "0x11e1a300".into(),
        difficulty: "0x0".into(),
        mixHash: "0x0000000000000000000000000000000000000000000000000000000000000000".into(),
        coinbase: "0x0000000000000000000000000000000000000000".into(),
        stateRoot: "0x5eb6e371a698b8d68f665192350ffcecbbbf322916f4b51bd79bb6887da3f494".into(),
        number: "0x0".into(),
        alloc,
        gasUsed: "0x0".into(),
        parentHash: "0x0000000000000000000000000000000000000000000000000000000000000000".into(),
    };

    fs::write("genesis.json", serde_json::to_string_pretty(&genesis)?)?;
    println!("✅  genesis.json written with {NUM_ACCOUNTS} funded accounts");
    Ok(())
}
