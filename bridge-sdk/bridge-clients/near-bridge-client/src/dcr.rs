use crate::{NearBridgeClient, TransactionOptions};
use bridge_connector_common::result::{BridgeSdkError, Result};
use futures::future::join_all;

use near_primitives::{hash::CryptoHash, types::AccountId};
use near_rpc_client::{ChangeRequest, ViewRequest};
use near_sdk::json_types::U128;
use omni_types::{ChainKind, OmniAddress, TransferId};

use serde_json::{json, Value};
use serde_with::{serde_as, DisplayFromStr};

use std::collections::HashMap;

// ---------------------------
// DCR-SPECIFIC TYPES
// ---------------------------
use omni_types::dcr::{DcrTxOut, OutPoint};

use dcr_utils::UTXO;

#[serde_as]
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct DcrWithdrawBridgeFee {
    #[serde_as(as = "DisplayFromStr")]
    fee_min: u128,
    fee_rate: u64,
    protocol_fee_rate: u64,
}

#[serde_as]
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct DcrPartialConfig {
    withdraw_bridge_fee: DcrWithdrawBridgeFee,
    change_address: String,

    #[serde_as(as = "DisplayFromStr")]
    min_deposit_amount: u128,

    max_active_utxo_management_input_number: u8,
    max_active_utxo_management_output_number: u8,

    active_management_lower_limit: u32,
    active_management_upper_limit: u32,

    confirmations_strategy: HashMap<String, u8>,
    confirmations_delta: u8,
}

#[serde_as]
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct DcrPartialMetadata {
    pub current_utxos_num: u32,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct FinDcrTransferArgs {
    pub deposit_msg: Value,
    pub tx_bytes: Vec<u8>,
    pub tx_index: u64,
    pub tx_block_hash: String,
    pub merkle_proof: Vec<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DcrVerifyWithdrawArgs {
    pub tx_id: String,
    pub tx_block_hash: String,
    pub tx_index: u64,
    pub merkle_proof: Vec<String>,
}

// ---------------------------
// DCR GAS & DEPOSITS
// ---------------------------
const DCR_INIT_TRANSFER_GAS: u64 = 300_000_000_000_000;
const DCR_SUBMIT_TRANSFER_GAS: u64 = 300_000_000_000_000;
const DCR_SIGN_TX_GAS: u64 = 300_000_000_000_000;
const DCR_VERIFY_DEPOSIT_GAS: u64 = 300_000_000_000_000;
const DCR_VERIFY_WITHDRAW_GAS: u64 = 300_000_000_000_000;

const DCR_INIT_TRANSFER_DEPOSIT: u128 = 1;
const DCR_SUBMIT_TRANSFER_DEPOSIT: u128 = 0;
const DCR_SIGN_TX_DEPOSIT: u128 = 250_000_000_000_000_000_000_000;
const DCR_VERIFY_DEPOSIT_DEPOSIT: u128 = 0;
const DCR_VERIFY_WITHDRAW_DEPOSIT: u128 = 0;

// ---------------------------
// TokenReceiverMessage for DCR
// ---------------------------
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum DcrTokenReceiverMessage {
    DepositProtocolFee,
    Withdraw {
        target_dcr_address: String,
        input: Vec<OutPoint>,
        output: Vec<DcrTxOut>,
        max_fee_rate: Option<U128>,
    },
}

#[serde_as]
#[derive(Clone, serde::Serialize, serde::Deserialize, Debug)]
pub struct DcrPendingInfoPartial {
    pub account_id: AccountId,
    pub dcr_pending_id: String,

    #[serde_as(as = "DisplayFromStr")]
    pub transfer_amount: u128,

    #[serde_as(as = "DisplayFromStr")]
    pub actual_received_amount: u128,

    #[serde_as(as = "DisplayFromStr")]
    pub withdraw_fee: u128,

    #[serde_as(as = "DisplayFromStr")]
    pub burn_amount: u128,

    pub tx_bytes_with_sign: Option<Vec<u8>>,
}

// ---------------------------
// SDK IMPLEMENTATION
// ---------------------------
impl NearBridgeClient {
    async fn get_dcr_config(&self, chain: ChainKind) -> Result<DcrPartialConfig> {
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let response = near_rpc_client::view(
            endpoint,
            ViewRequest {
                contract_account_id: connector,
                method_name: "get_config".to_string(),
                args: serde_json::json!({}),
            },
        )
        .await?;

        Ok(serde_json::from_slice::<DcrPartialConfig>(&response)?)
    }
    // -----------------------------------------------------
    // 1. submit_transfer_to_dcr_connector (OmniBridge call)
    // -----------------------------------------------------
    pub async fn submit_dcr_transfer(
        &self,
        transfer_id: TransferId,
        msg: DcrTokenReceiverMessage,
        transaction_options: TransactionOptions,
    ) -> Result<CryptoHash> {
        let endpoint = self.endpoint()?;
        let omni_bridge = self.omni_bridge_id()?;

        let tx_hash = near_rpc_client::change_and_wait(
            endpoint,
            ChangeRequest {
                signer: self.signer()?,
                nonce: transaction_options.nonce,
                receiver_id: omni_bridge,
                method_name: "submit_transfer_to_dcr_connector".to_string(),
                args: serde_json::json!({
                    "transfer_id": transfer_id,
                    "msg": json!(msg).to_string(),
                })
                .to_string()
                .into_bytes(),
                gas: DCR_SUBMIT_TRANSFER_GAS,
                deposit: DCR_SUBMIT_TRANSFER_DEPOSIT,
            },
            transaction_options.wait_until,
            transaction_options.wait_final_outcome_timeout_sec,
        )
        .await?;

        Ok(tx_hash)
    }

    // -----------------------------------------------------
    // 2. init_dcr_transfer (near ft_transfer_call)
    // -----------------------------------------------------
    pub async fn init_dcr_transfer(
        &self,
        chain: ChainKind,
        amount: u128,
        msg: DcrTokenReceiverMessage,
        transaction_options: TransactionOptions,
    ) -> Result<CryptoHash> {
        let endpoint = self.endpoint()?;

        let dcr_connector = self.utxo_chain_connector(chain)?;
        let dcr_token = self.utxo_chain_token(chain)?;

        let tx_hash = near_rpc_client::change_and_wait(
            endpoint,
            ChangeRequest {
                signer: self.signer()?,
                nonce: transaction_options.nonce,
                receiver_id: dcr_token,
                method_name: "ft_transfer_call".to_string(),
                args: serde_json::json!({
                    "receiver_id": dcr_connector,
                    "amount": amount.to_string(),
                    "msg": json!(msg).to_string(),
                })
                .to_string()
                .into_bytes(),
                gas: DCR_INIT_TRANSFER_GAS,
                deposit: DCR_INIT_TRANSFER_DEPOSIT,
            },
            transaction_options.wait_until,
            transaction_options.wait_final_outcome_timeout_sec,
        )
        .await?;

        Ok(tx_hash)
    }

    // -----------------------------------------------------
    // 3. sign_dcr_transaction
    // -----------------------------------------------------
    pub async fn sign_dcr_transaction(
        &self,
        chain: ChainKind,
        dcr_pending_id: String,
        sign_index: u64,
        transaction_options: TransactionOptions,
    ) -> Result<CryptoHash> {
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let tx_hash = near_rpc_client::change_and_wait(
            endpoint,
            ChangeRequest {
                signer: self.signer()?,
                nonce: transaction_options.nonce,
                receiver_id: connector,
                method_name: "sign_dcr_transaction".to_string(),
                args: serde_json::json!({
                    "dcr_pending_sign_id": dcr_pending_id,
                    "sign_index": sign_index,
                    "key_version": 0,
                })
                .to_string()
                .into_bytes(),
                gas: DCR_SIGN_TX_GAS,
                deposit: DCR_SIGN_TX_DEPOSIT,
            },
            transaction_options.wait_until,
            transaction_options.wait_final_outcome_timeout_sec,
        )
        .await?;

        Ok(tx_hash)
    }

    // -----------------------------------------------------
    // 4. verify_deposit (DCR)
    // -----------------------------------------------------
    pub async fn dcr_verify_deposit(
        &self,
        chain: ChainKind,
        args: Value, // <-- Replace with real DCR STRUCT when ready
        transaction_options: TransactionOptions,
    ) -> Result<CryptoHash> {
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let tx_hash = near_rpc_client::change_and_wait(
            endpoint,
            ChangeRequest {
                signer: self.signer()?,
                nonce: transaction_options.nonce,
                receiver_id: connector,
                method_name: "verify_dcr_deposit".to_string(),
                args: serde_json::json!(args).to_string().into_bytes(),
                gas: DCR_VERIFY_DEPOSIT_GAS,
                deposit: DCR_VERIFY_DEPOSIT_DEPOSIT,
            },
            transaction_options.wait_until,
            transaction_options.wait_final_outcome_timeout_sec,
        )
        .await?;

        Ok(tx_hash)
    }

    // -----------------------------------------------------
    // 6. get_dcr_pending_info
    // -----------------------------------------------------
    pub async fn get_dcr_pending_info(
        &self,
        chain: ChainKind,
        dcr_pending_id: String,
    ) -> Result<DcrPendingInfoPartial> {
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let response = near_rpc_client::view(
            endpoint,
            ViewRequest {
                contract_account_id: connector,
                method_name: "list_dcr_pending_infos".to_string(),
                args: serde_json::json!({
                    "dcr_pending_ids": [dcr_pending_id]
                }),
            },
        )
        .await?;

        let map =
            serde_json::from_slice::<HashMap<String, Option<DcrPendingInfoPartial>>>(&response)?;

        Ok(map
            .get(&dcr_pending_id)
            .cloned()
            .flatten()
            .ok_or(BridgeSdkError::InvalidArgument(
                "DCR pending info not found".to_string(),
            ))?)
    }

    // -----------------------------------------------------
    // 7. get_dcr_address (for deposit from DCR → NEAR)
    // -----------------------------------------------------
    pub async fn get_dcr_address(
        &self,
        chain: ChainKind,
        recipient: OmniAddress,
        fee: u128,
    ) -> Result<String> {
        let deposit_msg = self.get_deposit_msg_for_omni_bridge(recipient, fee)?;
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let response = near_rpc_client::view(
            endpoint,
            ViewRequest {
                contract_account_id: connector,
                method_name: "get_user_deposit_address".to_string(),
                args: serde_json::json!({
                    "deposit_msg": deposit_msg
                }),
            },
        )
        .await?;

        Ok(serde_json::from_slice::<String>(&response)?)
    }

    pub async fn get_dcr_utxos(&self, chain: ChainKind) -> Result<HashMap<String, UTXO>> {
        const UTXO_BATCH_SIZE: u32 = 500;

        let utxo_num = self.get_dcr_utxo_num(chain).await?;
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let batch_num = utxo_num.div_ceil(UTXO_BATCH_SIZE);
        let mut futures = Vec::new();

        for i in 0..batch_num {
            futures.push(near_rpc_client::view(
                endpoint,
                ViewRequest {
                    contract_account_id: connector.clone(),
                    method_name: "get_utxos_paged".to_string(),
                    args: serde_json::json!({
                        "from_index": i * UTXO_BATCH_SIZE,
                        "limit": UTXO_BATCH_SIZE
                    }),
                },
            ));
        }

        let responses = join_all(futures).await;
        let mut utxos = HashMap::new();

        for resp in responses {
            let part: HashMap<String, UTXO> = serde_json::from_slice(&resp?)?;
            utxos.extend(part);
        }

        Ok(utxos)
    }

    pub async fn get_dcr_change_address(&self, chain: ChainKind) -> Result<String> {
        let config = self.get_dcr_config(chain).await?;
        Ok(config.change_address)
    }

    pub async fn get_dcr_withdraw_fee(&self, chain: ChainKind) -> Result<u128> {
        let config = self.get_dcr_config(chain).await?;
        Ok(config.withdraw_bridge_fee.fee_min)
    }

    pub async fn get_dcr_active_management_limit(
        &self,
        chain: ChainKind,
    ) -> Result<(u32, u32, u8, u8)> {
        let config = self.get_dcr_config(chain).await?;
        Ok((
            config.active_management_lower_limit,
            config.active_management_upper_limit,
            config.max_active_utxo_management_input_number,
            config.max_active_utxo_management_output_number,
        ))
    }

    pub async fn get_dcr_min_deposit_amount(&self, chain: ChainKind) -> Result<u128> {
        let config = self.get_dcr_config(chain).await?;
        Ok(config.min_deposit_amount)
    }

    pub async fn get_dcr_amount_to_transfer(&self, chain: ChainKind, amount: u128) -> Result<u128> {
        let config = self.get_dcr_config(chain).await?;
        Ok(std::cmp::max(amount, config.min_deposit_amount))
    }

    pub async fn get_dcr_confirmations(&self, chain: ChainKind) -> Result<u8> {
        let config = self.get_dcr_config(chain).await?;

        Ok(config
            .confirmations_strategy
            .values()
            .max()
            .copied()
            .unwrap_or(0)
            + config.confirmations_delta)
    }

    pub async fn get_dcr_utxo_num(&self, chain: ChainKind) -> Result<u32> {
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let response = near_rpc_client::view(
            endpoint,
            ViewRequest {
                contract_account_id: connector,
                method_name: "get_metadata".to_string(),
                args: serde_json::json!({}),
            },
        )
        .await?;

        let metadata = serde_json::from_slice::<DcrPartialMetadata>(&response)?;
        Ok(metadata.current_utxos_num)
    }

    pub async fn sign_dcr_transaction_with_tx_hash(
        &self,
        chain: ChainKind,
        near_tx_hash: CryptoHash,
        user_account_id: Option<AccountId>,
        sign_index: u64,
        transaction_options: TransactionOptions,
    ) -> Result<CryptoHash> {
        let relayer_id = match user_account_id {
            Some(id) => id,
            None => self.satoshi_relayer(chain)?,
        };

        let log = self
            .extract_transfer_log(near_tx_hash, Some(relayer_id), "generate_dcr_pending_info")
            .await?;

        let json_str = log
            .strip_prefix("EVENT_JSON:")
            .ok_or(BridgeSdkError::InvalidLog(
                "Missing EVENT_JSON prefix".to_string(),
            ))?;

        let v: Value = serde_json::from_str(json_str)?;
        let dcr_pending_id = v["data"][0]["dcr_pending_id"]
            .as_str()
            .ok_or(BridgeSdkError::InvalidLog(
                "dcr_pending_id not found".to_string(),
            ))?
            .to_string();

        self.sign_dcr_transaction(chain, dcr_pending_id, sign_index, transaction_options)
            .await
    }

    #[tracing::instrument(skip_all, name = "NEAR FIN DCR TRANSFER")]
    pub async fn fin_dcr_transfer(
        &self,
        chain: ChainKind,
        args: FinDcrTransferArgs,
        transaction_options: TransactionOptions,
    ) -> Result<CryptoHash> {
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let tx_hash = near_rpc_client::change_and_wait(
            endpoint,
            ChangeRequest {
                signer: self.signer()?,
                nonce: transaction_options.nonce,
                receiver_id: connector,
                method_name: "verify_dcr_deposit".to_string(),
                args: serde_json::json!(args).to_string().into_bytes(),
                gas: DCR_VERIFY_DEPOSIT_GAS,
                deposit: DCR_VERIFY_DEPOSIT_DEPOSIT,
            },
            transaction_options.wait_until,
            transaction_options.wait_final_outcome_timeout_sec,
        )
        .await?;

        Ok(tx_hash)
    }

    #[tracing::instrument(skip_all, name = "NEAR DCR VERIFY WITHDRAW")]
    pub async fn dcr_verify_withdraw(
        &self,
        chain: ChainKind,
        args: DcrVerifyWithdrawArgs,
        transaction_options: TransactionOptions,
    ) -> Result<CryptoHash> {
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let tx_hash = near_rpc_client::change_and_wait(
            endpoint,
            ChangeRequest {
                signer: self.signer()?,
                nonce: transaction_options.nonce,
                receiver_id: connector,
                method_name: "verify_dcr_withdraw".to_string(),
                args: serde_json::json!(args).to_string().into_bytes(),
                gas: DCR_VERIFY_WITHDRAW_GAS,
                deposit: DCR_VERIFY_WITHDRAW_DEPOSIT,
            },
            transaction_options.wait_until,
            transaction_options.wait_final_outcome_timeout_sec,
        )
        .await?;

        Ok(tx_hash)
    }

    #[tracing::instrument(skip_all, name = "NEAR DCR CANCEL WITHDRAW")]
    pub async fn dcr_cancel_withdraw(
        &self,
        chain: ChainKind,
        dcr_tx_hash: String,
        transaction_options: TransactionOptions,
    ) -> Result<CryptoHash> {
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let tx_hash = near_rpc_client::change_and_wait(
            endpoint,
            ChangeRequest {
                signer: self.signer()?,
                nonce: transaction_options.nonce,
                receiver_id: connector,
                method_name: "cancel_withdraw".to_string(),
                args: serde_json::json!({
                    "original_dcr_pending_verify_id": dcr_tx_hash,
                    "output": [],
                })
                .to_string()
                .into_bytes(),
                gas: DCR_VERIFY_WITHDRAW_GAS,
                deposit: 0,
            },
            transaction_options.wait_until,
            transaction_options.wait_final_outcome_timeout_sec,
        )
        .await?;

        Ok(tx_hash)
    }

    #[tracing::instrument(skip_all, name = "NEAR DCR VERIFY ACTIVE UTXO MANAGEMENT")]
    pub async fn dcr_verify_active_utxo_management(
        &self,
        chain: ChainKind,
        args: DcrVerifyWithdrawArgs,
        transaction_options: TransactionOptions,
    ) -> Result<CryptoHash> {
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let tx_hash = near_rpc_client::change_and_wait(
            endpoint,
            ChangeRequest {
                signer: self.signer()?,
                nonce: transaction_options.nonce,
                receiver_id: connector,
                method_name: "verify_active_utxo_management".to_string(),
                args: serde_json::json!(args).to_string().into_bytes(),
                gas: DCR_VERIFY_WITHDRAW_GAS,
                deposit: 0,
            },
            transaction_options.wait_until,
            transaction_options.wait_final_outcome_timeout_sec,
        )
        .await?;

        Ok(tx_hash)
    }

    pub async fn active_utxo_management_dcr(
        &self,
        chain: ChainKind,
        input: Vec<OutPoint>,
        output: Vec<DcrTxOut>,
        transaction_options: TransactionOptions,
    ) -> Result<CryptoHash> {
        let endpoint = self.endpoint()?;
        let connector = self.utxo_chain_connector(chain)?;

        let tx_hash = near_rpc_client::change_and_wait(
            endpoint,
            ChangeRequest {
                signer: self.signer()?,
                nonce: transaction_options.nonce,
                receiver_id: connector,
                method_name: "active_utxo_management".to_string(),
                args: serde_json::json!({
                    "input": input,
                    "output": output,
                })
                .to_string()
                .into_bytes(),
                gas: 300_000_000_000_000,
                deposit: 0,
            },
            transaction_options.wait_until,
            transaction_options.wait_final_outcome_timeout_sec,
        )
        .await?;

        Ok(tx_hash)
    }
}
