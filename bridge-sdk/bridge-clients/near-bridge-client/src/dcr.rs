use crate::{NearBridgeClient, TransactionOptions};
use bridge_connector_common::result::{BridgeSdkError, Result};
use futures::future::join_all;

use near_primitives::{hash::CryptoHash, types::AccountId};
use near_rpc_client::{ChangeRequest, ViewRequest};
use near_sdk::json_types::{U128, U64};
use omni_types::{ChainKind, OmniAddress, TransferId};

use serde_json::{json, Value};
use serde_with::{serde_as, DisplayFromStr};

use std::collections::HashMap;

// ---------------------------
// DCR-SPECIFIC TYPES
// ---------------------------
use omni_types::dcr::{OutPoint, DcrTxOut};

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
    // 5. verify_withdraw (DCR)
    // -----------------------------------------------------
    pub async fn dcr_verify_withdraw(
        &self,
        chain: ChainKind,
        args: Value, // <--- replace with DcrWithdrawArgs
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

        let map = serde_json::from_slice::<HashMap<String, Option<DcrPendingInfoPartial>>>(
            &response,
        )?;

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
}
