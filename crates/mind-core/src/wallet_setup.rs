//! Operator-only custody bootstrap for the real-money Base wallet.
//!
//! This module is intentionally outside `mind-conversation` and the agent tool registry. It can
//! create and recover one EVM key, but it cannot build, sign, or broadcast a transaction. The key
//! lives in the current Windows user's Credential Manager and is never written to Mind memory,
//! repository files, environment variables, command arguments, or logs.

#[cfg(windows)]
use std::io::{BufRead, IsTerminal, Write};

use anyhow::bail;
#[cfg(windows)]
use anyhow::Context;
#[cfg(windows)]
use k256::{elliptic_curve::sec1::ToEncodedPoint, SecretKey};
#[cfg(windows)]
use rand_core::OsRng;
#[cfg(windows)]
use sha3::{Digest, Keccak256};
#[cfg(windows)]
use zeroize::{Zeroize, Zeroizing};

#[cfg(windows)]
const CHAIN_NAME: &str = "Base Mainnet";
#[cfg(windows)]
const CHAIN_ID: u64 = 8453;
#[cfg(windows)]
const KEY_SERVICE: &str = "yantrik-mind.wallet.v1";
#[cfg(windows)]
const KEY_ACCOUNT: &str = "base-mainnet-primary";
#[cfg(windows)]
const BACKUP_SERVICE: &str = "yantrik-mind.wallet-backup.v1";

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct WalletIdentity {
    address: String,
    backup_verified: bool,
}

#[cfg(windows)]
fn address_from_secret(secret: &[u8]) -> anyhow::Result<String> {
    let secret = SecretKey::from_slice(secret).context("stored wallet key is invalid")?;
    let public = secret.public_key().to_encoded_point(false);
    let bytes = public.as_bytes();
    if bytes.len() != 65 || bytes[0] != 4 {
        bail!("wallet public key has an unexpected encoding");
    }
    let hash = Keccak256::digest(&bytes[1..]);
    Ok(eip55(&hex::encode(&hash[12..])))
}

#[cfg(windows)]
fn eip55(lower_hex: &str) -> String {
    debug_assert_eq!(lower_hex.len(), 40);
    let hash = Keccak256::digest(lower_hex.as_bytes());
    let mut out = String::with_capacity(42);
    out.push_str("0x");
    for (index, ch) in lower_hex.bytes().enumerate() {
        let nibble = if index % 2 == 0 {
            hash[index / 2] >> 4
        } else {
            hash[index / 2] & 0x0f
        };
        if ch.is_ascii_alphabetic() && nibble >= 8 {
            out.push((ch as char).to_ascii_uppercase());
        } else {
            out.push(ch as char);
        }
    }
    out
}

#[cfg(windows)]
fn key_entry() -> anyhow::Result<keyring::Entry> {
    keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT).context("open Windows wallet credential")
}

#[cfg(windows)]
fn backup_entry() -> anyhow::Result<keyring::Entry> {
    keyring::Entry::new(BACKUP_SERVICE, KEY_ACCOUNT).context("open Windows backup marker")
}

#[cfg(windows)]
fn read_key() -> anyhow::Result<Option<Zeroizing<Vec<u8>>>> {
    match key_entry()?.get_secret() {
        Ok(secret) => {
            let secret = Zeroizing::new(secret);
            if secret.len() != 32 {
                bail!("stored wallet key has the wrong length; refusing to replace it");
            }
            Ok(Some(secret))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error).context("read wallet from Windows Credential Manager"),
    }
}

#[cfg(windows)]
fn backup_verified(address: &str) -> anyhow::Result<bool> {
    match backup_entry()?.get_password() {
        Ok(marked_address) => Ok(marked_address == address),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(error) => Err(error).context("read wallet backup marker"),
    }
}

#[cfg(windows)]
fn identity() -> anyhow::Result<Option<WalletIdentity>> {
    let Some(secret) = read_key()? else {
        return Ok(None);
    };
    let address = address_from_secret(&secret)?;
    Ok(Some(WalletIdentity {
        backup_verified: backup_verified(&address)?,
        address,
    }))
}

#[cfg(windows)]
fn create() -> anyhow::Result<()> {
    if identity()?.is_some() {
        println!("Wallet already exists; nothing changed.");
        return print_status();
    }

    let secret = SecretKey::random(&mut OsRng);
    let mut bytes = secret.to_bytes();
    let address = address_from_secret(bytes.as_slice())?;
    key_entry()?
        .set_secret(bytes.as_slice())
        .context("store wallet in Windows Credential Manager")?;
    bytes.as_mut_slice().zeroize();

    println!("Created one {CHAIN_NAME} wallet in Windows Credential Manager.");
    println!("Receive address: WITHHELD until recovery backup is verified.");
    println!("No signer or transaction broadcaster was enabled.");
    println!("Next: run `ym wallet backup` in your own local terminal.");
    println!("Do not send funds yet.");
    // Deriving before storage also proves the generated key had a valid public identity. Keep the
    // address out of output until the human has demonstrated a recovery copy.
    let _ = address;
    Ok(())
}

#[cfg(windows)]
fn print_status() -> anyhow::Result<()> {
    match identity()? {
        None => println!("No wallet exists. Run `ym wallet create` locally."),
        Some(wallet) if !wallet.backup_verified => {
            println!("Wallet: created for {CHAIN_NAME} (chain ID {CHAIN_ID})");
            println!("Recovery backup: NOT VERIFIED");
            println!("Receive address: WITHHELD");
            println!(
                "Next: run `ym wallet backup` in your own local terminal. Do not fund it yet."
            );
        }
        Some(wallet) => {
            println!("Wallet: READY for receiving on {CHAIN_NAME} (chain ID {CHAIN_ID})");
            println!("Receive address: {}", wallet.address);
            println!("Execution: LOCKED — no signer or broadcaster is connected to Mind");
            println!(
                "Fund only on Base, and start with a tiny test transfer plus enough ETH for gas."
            );
        }
    }
    Ok(())
}

#[cfg(windows)]
fn backup() -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("wallet backup requires a private interactive terminal; refusing redirected or captured output");
    }
    let Some(secret) = read_key()? else {
        bail!("no wallet exists; run `ym wallet create` first");
    };
    let address = address_from_secret(&secret)?;
    if backup_verified(&address)? {
        println!("Recovery backup is already verified. Nothing was revealed.");
        return Ok(());
    }

    println!("This will reveal the wallet private key ONCE in this terminal.");
    println!("Anyone with it can take every asset. Never paste it into chat, email, cloud notes, or Mind.");
    print!("Type SHOW to continue: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if line.trim() != "SHOW" {
        bail!("backup cancelled; nothing was revealed");
    }

    let key_hex = Zeroizing::new(hex::encode(secret.as_slice()));
    println!("\nPRIVATE KEY (store offline): 0x{}", key_hex.as_str());
    println!("PUBLIC ADDRESS: {address}\n");
    print!("After saving it offline, type the final 8 private-key characters to verify: ");
    std::io::stdout().flush()?;
    line.clear();
    std::io::stdin().lock().read_line(&mut line)?;
    let expected = &key_hex[key_hex.len() - 8..];
    let verified = line.trim() == expected;
    line.zeroize();
    if !verified {
        bail!("backup verification failed; the receive address remains locked");
    }
    backup_entry()?
        .set_password(&address)
        .context("store wallet backup marker")?;
    println!("Backup verified. The wallet is now READY to receive on {CHAIN_NAME} only.");
    println!("Receive address: {address}");
    Ok(())
}

#[cfg(windows)]
fn receive() -> anyhow::Result<()> {
    let Some(wallet) = identity()? else {
        bail!("no wallet exists; run `ym wallet create` first");
    };
    if !wallet.backup_verified {
        bail!("receive address is locked until `ym wallet backup` is completed locally");
    }
    println!("{}", wallet.address);
    Ok(())
}

#[cfg(not(windows))]
fn unsupported() -> anyhow::Result<()> {
    bail!("secure wallet provisioning currently requires Windows Credential Manager")
}

/// Run the trusted local wallet bootstrap. This function is intentionally synchronous and executes
/// before the rest of Mind starts.
pub fn run(args: impl IntoIterator<Item = String>) -> anyhow::Result<()> {
    let args: Vec<String> = args.into_iter().collect();
    let command = args.first().map(String::as_str).unwrap_or("status");
    if args.len() > 1 {
        bail!("wallet commands accept no extra arguments (secrets must never enter process arguments)");
    }
    #[cfg(windows)]
    {
        match command {
            "create" => create(),
            "status" => print_status(),
            "backup" => backup(),
            "receive" | "address" => receive(),
            _ => bail!("usage: ym wallet <create|status|backup|receive>"),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = command;
        unsupported()
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn private_key_one_derives_the_canonical_ethereum_address() {
        let mut key = [0_u8; 32];
        key[31] = 1;
        assert_eq!(
            address_from_secret(&key).unwrap(),
            "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf"
        );
    }

    #[test]
    fn invalid_private_keys_fail_closed() {
        assert!(address_from_secret(&[0_u8; 31]).is_err());
        assert!(address_from_secret(&[0_u8; 32]).is_err());
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;

    #[test]
    fn command_arguments_can_never_carry_wallet_secrets() {
        let error = run(["status".to_string(), "secret".to_string()]).unwrap_err();
        assert!(error.to_string().contains("no extra arguments"), "{error}");
    }
}
