use alloy::signers::wallet::coins_bip39::{English, Mnemonic};
use eyre::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Generate a random wallet (24 word phrase)
    let phrase = Mnemonic::<English>::new_with_count(&mut rand::thread_rng(), 24)?;
    println!("{}", phrase.to_phrase());
    Ok(())
}
