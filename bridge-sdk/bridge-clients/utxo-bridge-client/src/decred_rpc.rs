use hex::FromHex;
use reqwest::Client;
use serde::Deserialize;

use crate::error::UtxoClientError;
use crate::types::DecredBlock;

use merkle_tools::H256;

#[derive(Clone)]
pub struct DecredRpc {
    pub url: String,
    pub user: String,
    pub pass: String,
    client: Client,
}

impl DecredRpc {
    pub fn new(url: String, user: String, pass: String) -> Self {
        Self {
            url,
            user,
            pass,
            client: Client::new(),
        }
    }

    async fn rpc_call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T, UtxoClientError> {
        let body = serde_json::json!({
            "jsonrpc": "1.0",
            "id": "dcr",
            "method": method,
            "params": params
        });

        let resp = self
            .client
            .post(&self.url)
            .basic_auth(&self.user, Some(&self.pass))
            .json(&body)
            .send()
            .await
            .map_err(|e| UtxoClientError::Other(format!("RPC error: {e}")))?;

        let json: RpcResponse<T> = resp
            .json()
            .await
            .map_err(|e| UtxoClientError::Other(format!("Invalid RPC response: {e}")))?;

        if let Some(err) = json.error {
            return Err(UtxoClientError::Other(format!(
                "RPC returned error: {}",
                err.message
            )));
        }

        Ok(json.result)
    }

    // -----------------------------
    // Get raw transaction hex
    // -----------------------------
    pub async fn get_raw_tx(&self, txid: &str) -> Result<DcrRawTx, UtxoClientError> {
        self.rpc_call("getrawtransaction", serde_json::json!([txid, 0]))
            .await
    }

    // -----------------------------
    // Get block + TX list
    // -----------------------------
    pub async fn get_block(&self, block_hash: &str) -> Result<DecredBlock, UtxoClientError> {
        let block: GetBlockResult = self
            .rpc_call("getblock", serde_json::json!([block_hash, 2]))
            .await?;

        // convert txids to H256
        let txids: Vec<H256> = block
            .tx
            .iter()
            .map(|tx| {
                let bytes = <[u8; 32]>::from_hex(tx.txid.clone()).unwrap();
                H256::from(bytes)
            })
            .collect();

        // fetch raw tx hex
        let mut tx_hex = Vec::new();
        for tx in &block.tx {
            let raw = self.get_raw_tx(&tx.txid).await?;
            tx_hex.push(raw.hex);
        }

        Ok(DecredBlock {
            hash: block.hash,
            height: block.height,
            txids,
            tx_hex,
        })
    }

    // -----------------------------
    // Get merkle proof
    // -----------------------------
    pub async fn get_merkle_proof(
        &self,
        txid: &str,
        block_hash: &str,
    ) -> Result<DecredMerkleProof, UtxoClientError> {
        self.rpc_call("getmerkleproof", serde_json::json!([txid, block_hash]))
            .await
    }
    pub async fn send_dcr_transaction(&self, tx_bytes: &[u8]) -> Result<String, UtxoClientError> {
        let hex_str = hex::encode(tx_bytes);

        let body = serde_json::json!({
            "jsonrpc": "1.0",
            "id": "dcr",
            "method": "sendrawtransaction",
            "params": [hex_str]
        });

        let resp = self
            .client
            .post(&self.url)
            .basic_auth(&self.user, Some(&self.pass))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                UtxoClientError::RpcError(format!("DCR sendrawtransaction failed: {e}"))
            })?;

        let json: serde_json::Value = resp.json().await.map_err(|e| {
            UtxoClientError::RpcError(format!("Invalid DCR sendrawtransaction response: {e}"))
        })?;

        if json.get("error").is_some() && !json["error"].is_null() {
            return Err(UtxoClientError::RpcError(format!(
                "DCR sendrawtransaction returned error: {:#?}",
                json["error"]
            )));
        }

        let result = json["result"]
            .as_str()
            .ok_or(UtxoClientError::RpcError(
                "Missing txid in DCR result".into(),
            ))?
            .to_string();

        Ok(result)
    }
}

//
// RPC BASE STRUCTS
//

#[derive(Debug, Deserialize)]
struct RpcResponse<T> {
    pub result: T,
    pub error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    #[allow(dead_code)]
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct DcrRawTx {
    pub txid: String,

    #[serde(rename = "blockhash")]
    pub block_hash: Option<String>,

    pub hex: String,
}

//
// ─── GETBLOCK RPC STRUCTS ─────────────────────────────────────────
//

#[derive(Debug, Deserialize)]
pub struct GetBlockResult {
    pub hash: String,
    pub height: u64,
    pub tx: Vec<GetBlockTransaction>,
}

#[derive(Debug, Deserialize)]
pub struct GetBlockTransaction {
    pub txid: String,
}

//
// ─── MERKLE PROOF ──────────────────────────────────────────────────
//

#[derive(Debug, Deserialize)]
pub struct DecredMerkleProof {
    pub block_hash: String,
    pub tx_index: u64,
    pub hashes: Vec<String>,
}
