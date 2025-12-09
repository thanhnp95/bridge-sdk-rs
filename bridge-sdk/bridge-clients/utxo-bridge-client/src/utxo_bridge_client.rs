use bitcoin::BlockHash;
use bitcoin::hashes::Hash;
use bitcoincore_rpc::json::EstimateSmartFeeResult;
use bitcoincore_rpc::{bitcoin, jsonrpc::base64};
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Client, ClientBuilder,
};
use serde_json::{json, Value};
use std::{marker::PhantomData, str::FromStr};

use crate::error::UtxoClientError;
use crate::types::{TxProof, UTXOChain, UTXOChainBlock};
use crate::decred_rpc::DecredRpc;

pub mod error;
pub mod types;
pub mod decred_rpc;

pub enum AuthOptions {
    None,
    XApiKey(String),
    BasicAuth(String, String),
}

#[allow(dead_code)]
#[derive(serde::Deserialize, Debug)]
struct JsonRpcResponse<T> {
    jsonrpc: String,
    id: u64,
    result: T,
}

pub struct UTXOBridgeClient<T: UTXOChain> {
    endpoint_url: String,
    http_client: Client,

    /// Optional: Decred RPC (only created if chain is Decred)
    dcr_rpc: Option<DecredRpc>,

    _phantom: PhantomData<T>,
}

impl<T: UTXOChain> UTXOBridgeClient<T> {
    pub fn new(rpc_endpoint: String, auth: AuthOptions) -> Self {
        let mut headers = HeaderMap::new();

        match auth {
            AuthOptions::None => {}
            AuthOptions::XApiKey(api_key) => {
                headers.insert("x-api-key", HeaderValue::from_str(&api_key).unwrap());
            }
            AuthOptions::BasicAuth(username, password) => {
                let auth_value =
                    format!("Basic {}", base64::encode(format!("{username}:{password}")));
                headers.insert("Authorization", HeaderValue::from_str(&auth_value).unwrap());
            }
        }

        let http_client = ClientBuilder::new()
            .default_headers(headers)
            .build()
            .unwrap();

        // If Decred → initialize DecredRpc
        let dcr_rpc = if T::is_decred() {
            Some(DecredRpc::new(
                rpc_endpoint.clone(),
                "rpcuser".into(),
                "rpcpass".into(),
            ))
        } else {
            None
        };

        Self {
            endpoint_url: rpc_endpoint,
            http_client,
            dcr_rpc,
            _phantom: PhantomData,
        }
    }

    pub async fn get_block_hash_by_tx_hash(
        &self,
        tx_hash: &str,
    ) -> Result<BlockHash, UtxoClientError> {
        if T::is_decred() {
            return Ok(BlockHash::from_byte_array([0u8; 32]));
        }
        let args = if T::is_zcash() {
            json!([tx_hash, 1])
        } else {
            json!([tx_hash, true])
        };

        let response_text = self
            .http_client
            .post(&self.endpoint_url)
            .json(&json!({
                "id": 1,
                "jsonrpc": "2.0",
                "method": "getrawtransaction",
                "params": args
            }))
            .send()
            .await
            .map_err(|e| {
                UtxoClientError::RpcError(format!("Failed to send getrawtransaction request: {e}"))
            })?
            .text()
            .await
            .map_err(|e| {
                UtxoClientError::RpcError(format!("Failed to read getrawtransaction response: {e}"))
            })?;

        let response = serde_json::from_str::<Value>(&response_text).map_err(|_| {
            UtxoClientError::RpcError(format!(
                "Failed to read getrawtransaction. Response: {response_text}"
            ))
        })?;

        let result: Value = serde_json::from_value(response["result"].clone()).map_err(|e| {
            UtxoClientError::RpcError(format!(
                "Failed to parse getrawtransaction result: {e}. Response: {response_text}"
            ))
        })?;

        let hash_str = result["blockhash"].as_str().ok_or_else(|| {
            UtxoClientError::RpcError(format!(
                "Block hash not found in transaction data. Response: {response_text}"
            ))
        })?;

        let receipt = BlockHash::from_str(hash_str).map_err(|e| {
            UtxoClientError::RpcError(format!(
                "Block hash parsing error: {e}. Response: {response_text}"
            ))
        })?;

        Ok(receipt)
    }

    pub async fn get_block_height_by_block_hash(
        &self,
        block_hash: &str,
    ) -> Result<u64, UtxoClientError> {

        if T::is_decred() {
            let dcr = self.dcr_rpc.as_ref().unwrap();
            let block = dcr.get_block(block_hash).await?;
            return Ok(block.height);
        }

        let response_text = self
            .http_client
            .post(&self.endpoint_url)
            .json(&json!({
                "id": 1,
                "jsonrpc": "2.0",
                "method": "getblockheader",
                "params": [block_hash.to_string(), true],
            }))
            .send()
            .await
            .map_err(|e| {
                UtxoClientError::RpcError(format!("Failed to send getblock request: {e}"))
            })?
            .text()
            .await
            .map_err(|e| {
                UtxoClientError::RpcError(format!("Failed to read getblock response: {e}"))
            })?;

        let response = serde_json::from_str::<Value>(&response_text).map_err(|_| {
            UtxoClientError::RpcError(format!(
                "Failed to send getblock. Response: {response_text}"
            ))
        })?;

        let result: Value = serde_json::from_value(response["result"].clone()).map_err(|e| {
            UtxoClientError::RpcError(format!(
                "Failed to parse send getblock result: {e}. Response: {response_text}"
            ))
        })?;

        let block_height = result["height"].as_u64().ok_or_else(|| {
            UtxoClientError::RpcError(format!("Block height not found. Response: {response_text}"))
        })?;

        Ok(block_height)
    }

    pub async fn extract_btc_proof(&self, tx_hash: &str) -> Result<TxProof, UtxoClientError> {
        if T::is_decred() {
            return self.extract_dcr_proof(tx_hash).await;
        }

        let block_hash = self.get_block_hash_by_tx_hash(tx_hash).await?;
        let block_height = self
            .get_block_height_by_block_hash(&block_hash.to_string())
            .await?;

        let response_text = self
            .http_client
            .post(&self.endpoint_url)
            .json(&json!({
                "id": 1,
                "jsonrpc": "2.0",
                "method": "getblock",
                "params": [block_hash.to_string(), 0],
            }))
            .send()
            .await
            .map_err(|e| {
                UtxoClientError::RpcError(format!("Failed to send getblock request: {e}"))
            })?
            .text()
            .await
            .map_err(|e| {
                UtxoClientError::RpcError(format!("Failed to read getblock response: {e}"))
            })?;

        let response = serde_json::from_str::<Value>(&response_text).map_err(|_| {
            UtxoClientError::RpcError(format!(
                "Failed to read getblock. Response: {response_text}"
            ))
        })?;

        let result: String = serde_json::from_value(response["result"].clone()).map_err(|e| {
            UtxoClientError::RpcError(format!(
                "Failed to parse read getblock result: {e}. Response: {response_text}"
            ))
        })?;

        let block = T::Block::from_str(&result)?;
        let transactions = block.transactions();

        let tx_index = transactions
            .iter()
            .position(|hash| hash.to_string() == tx_hash)
            .ok_or(UtxoClientError::Other(
                "btc tx not found in block".to_string(),
            ))?;

        let merkle_proof = merkle_tools::merkle_proof_calculator(transactions, tx_index);
        let merkle_proof_str = merkle_proof
            .iter()
            .map(std::string::ToString::to_string)
            .collect();

        Ok(TxProof {
            block_height,
            tx_bytes: block.tx_data(tx_index),
            tx_block_blockhash: block.hash(),
            tx_index: tx_index
                .try_into()
                .expect("Error on convert usize into u64"),
            merkle_proof: merkle_proof_str,
        })
    }

    // ============================================================
    // 4. DECRED PROOF
    // ============================================================
    pub async fn extract_dcr_proof(&self, tx_hash: &str) -> Result<TxProof, UtxoClientError> {
        let dcr = self.dcr_rpc.as_ref().unwrap();

        // Fetch tx info to get block hash
        let tx = dcr.get_raw_tx(tx_hash).await?;
        let block_hash = tx.block_hash.ok_or_else(|| UtxoClientError::Other("No blockhash".into()))?;

        // Fetch block
        let block = dcr.get_block(&block_hash).await?;

        // Tx index
        let tx_index = block
            .txids
            .iter()
            .position(|h| h.to_string() == tx_hash)
            .ok_or(UtxoClientError::Other("DCR tx not in block".into()))?;

        // Merkle proof
        let proof = dcr.get_merkle_proof(tx_hash, &block_hash).await?;
        let tx_bytes = hex::decode(&block.tx_hex[tx_index])
        .map_err(|e| UtxoClientError::Other(format!("Invalid DCR tx hex: {e}")))?;

        Ok(TxProof {
            block_height: block.height,
            tx_bytes,
            tx_block_blockhash: block_hash,
            tx_index: tx_index as u64,
            merkle_proof: proof.hashes,
        })
    }

    pub async fn get_fee_rate(&self) -> Result<u64, UtxoClientError> {
        if T::is_zcash() {
            return Ok(1000);
        }

        if T::is_decred() {
            // simple default fee rate for DCR
            return Ok(1000);
        }

        let response_text = self
            .http_client
            .post(&self.endpoint_url)
            .json(&json!({
                "id": 1,
                "jsonrpc": "2.0",
                "method": "estimatesmartfee",
                "params": [2]
            }))
            .send()
            .await
            .map_err(|e| {
                UtxoClientError::RpcError(format!("Failed to send estimatesmartfee request: {e}"))
            })?
            .text()
            .await
            .map_err(|e| {
                UtxoClientError::RpcError(format!("Failed to read estimatesmartfee response: {e}"))
            })?;

        let response = serde_json::from_str::<Value>(&response_text).map_err(|_| {
            UtxoClientError::RpcError(format!(
                "Failed to read estimatesmartfee. Response: {response_text}"
            ))
        })?;

        let result: EstimateSmartFeeResult = serde_json::from_value(response["result"].clone())
            .map_err(|e| {
                UtxoClientError::RpcError(format!(
                    "Failed to parse estimatesmartfee result: {e}. Response: {response_text}"
                ))
            })?;

        Ok(result
            .fee_rate
            .ok_or(UtxoClientError::RpcError(format!(
                "Failed to estimate fee_rate: {:?}",
                result.errors
            )))?
            .to_sat())
    }

    pub async fn send_tx(&self, tx_bytes: &[u8]) -> Result<String, UtxoClientError> {
        if T::is_decred() {
            let dcr = self.dcr_rpc.as_ref().unwrap();
            return dcr.send_dcr_transaction(tx_bytes).await;
        } 
        let hex_str = hex::encode(tx_bytes);
        let response_text = self
            .http_client
            .post(&self.endpoint_url)
            .json(&json!({
                "id": 1,
                "jsonrpc": "2.0",
                "method": "sendrawtransaction",
                "params": [hex_str]
            }))
            .send()
            .await
            .map_err(|e| UtxoClientError::RpcError(format!("Failed to send transaction: {e}")))?
            .text()
            .await
            .map_err(|e| {
                UtxoClientError::RpcError(format!(
                    "Failed to read sendrawtransaction response: {e}"
                ))
            })?;

        let response = serde_json::from_str::<Value>(&response_text).map_err(|_| {
            UtxoClientError::RpcError(format!(
                "Failed to read sendrawtransaction. Response: {response_text}"
            ))
        })?;

        let result: String = serde_json::from_value(response["result"].clone()).map_err(|e| {
            UtxoClientError::RpcError(format!(
                "Failed to parse sendrawtransaction result: {e}. Response: {response_text}"
            ))
        })?;

        Ok(result)
    }
}
