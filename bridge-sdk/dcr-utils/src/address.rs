use sha2::{Digest, Sha256};
use std::fmt;

/// DCR networks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Network {
    Mainnet,
    Testnet,
}

/// DCR Address variants
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DcrAddress {
    P2pkh { hash: [u8; 20], network: Network },
    P2sh { hash: [u8; 20], network: Network },
}

/* ---------------------------------------------------------
   Base58Check decode (BTC / Decred style)
--------------------------------------------------------- */
fn decode_base58_check(s: &str) -> Result<Vec<u8>, String> {
    let data = bs58::decode(s)
        .into_vec()
        .map_err(|e| format!("Base58 decode error: {e}"))?;

    if data.len() < 4 {
        return Err("Invalid Base58Check length".into());
    }

    let (payload, checksum) = data.split_at(data.len() - 4);

    let hash = Sha256::digest(&Sha256::digest(payload));

    if &hash[..4] != checksum {
        return Err("Base58 checksum mismatch".into());
    }

    Ok(payload.to_vec())
}

/* ---------------------------------------------------------
   Base58Check encode
--------------------------------------------------------- */
fn encode_base58_check(payload: &[u8]) -> String {
    let hash = Sha256::digest(&Sha256::digest(payload));

    let mut full = payload.to_vec();
    full.extend_from_slice(&hash[..4]); // append checksum

    bs58::encode(full).into_string()
}

/* ---------------------------------------------------------
   DcrAddress implementation
--------------------------------------------------------- */
impl DcrAddress {
    /// Parse DCR base58 address → DcrAddress enum
    pub fn parse(address: &str, network: Network) -> Result<Self, String> {
        let data = decode_base58_check(address)?;

        if data.len() < 22 {
            return Err("Invalid DCR address length".to_string());
        }

        let prefix = &data[0..2];
        let hash20: [u8; 20] = data[2..22]
            .try_into()
            .map_err(|_| "Failed to extract hash160".to_string())?;

        match (prefix, network) {
            // ---- MAINNET ----
            ([0x07, 0x3F], Network::Mainnet) => Ok(Self::P2pkh {
                hash: hash20,
                network,
            }),
            ([0x07, 0x1A], Network::Mainnet) => Ok(Self::P2sh {
                hash: hash20,
                network,
            }),

            // ---- TESTNET ----
            ([0x0F, 0x21], Network::Testnet) => Ok(Self::P2pkh {
                hash: hash20,
                network,
            }),
            ([0x0E, 0xE3], Network::Testnet) => Ok(Self::P2sh {
                hash: hash20,
                network,
            }),

            _ => Err(format!(
                "Unknown or mismatched DCR address prefix: {:02X?}",
                prefix
            )),
        }
    }

    /// Convert DCR address → pk_script (hex string)
    pub fn script_pubkey(&self) -> Result<String, String> {
        let script_bytes = match self {
            DcrAddress::P2pkh { hash, .. } => {
                // OP_DUP OP_HASH160 <20 bytes> OP_EQUALVERIFY OP_CHECKSIG
                let mut b = vec![0x76, 0xA9, 0x14];
                b.extend_from_slice(hash);
                b.extend_from_slice(&[0x88, 0xAC]);
                b
            }
            DcrAddress::P2sh { hash, .. } => {
                // OP_HASH160 <20 bytes> OP_EQUAL
                let mut b = vec![0xA9, 0x14];
                b.extend_from_slice(hash);
                b.push(0x87);
                b
            }
        };

        Ok(hex::encode(script_bytes))
    }
}

/* ---------------------------------------------------------
   Display (convert address struct → Base58Check string)
--------------------------------------------------------- */
impl fmt::Display for DcrAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (prefix, hash) = match self {
            DcrAddress::P2pkh { hash, network } => match network {
                Network::Mainnet => ([0x07, 0x3F], hash),
                Network::Testnet => ([0x0F, 0x21], hash),
            },
            DcrAddress::P2sh { hash, network } => match network {
                Network::Mainnet => ([0x07, 0x1A], hash),
                Network::Testnet => ([0x0E, 0xE3], hash),
            },
        };

        let mut payload = Vec::with_capacity(22);
        payload.extend_from_slice(&prefix);
        payload.extend_from_slice(hash);

        let addr = encode_base58_check(&payload);

        write!(f, "{}", addr)
    }
}
