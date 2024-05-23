use alloy::signers::wallet::{
    coins_bip39::{English, Mnemonic},
    MnemonicBuilder,
};
use eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Generate a random wallet (24 word phrase)
    let phrase = Mnemonic::<English>::new_with_count(&mut rand::thread_rng(), 24)?;
    println!("Mnemonic: {}", phrase.to_phrase());

    for i in 0..5 {
        let wallet = MnemonicBuilder::<English>::default()
            .phrase(phrase.to_phrase())
            .index(i)?
            .build()?;
        if i == 0 {
            println!("(master)Account {}: {}", i, wallet.address());
        } else {
            println!("Account {}: {}", i, wallet.address());
        }
    }
    Ok(())
}
