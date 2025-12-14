pub mod address;

use serde_with::{serde_as, DisplayFromStr};
use std::collections::HashMap;

// ---- IMPORT TYPES FROM omni-types ----
use omni_types::dcr::{DcrTxOut, OutPoint};

use crate::address::{DcrAddress, Network};

/// UTXO entry for DCR
#[serde_as]
#[derive(Clone, serde::Serialize, serde::Deserialize, Debug)]
pub struct UTXO {
    pub path: String,
    pub tx_bytes: Vec<u8>,
    pub vout: u32,
    #[serde_as(as = "DisplayFromStr")]
    pub balance: u64, // atoms
}

/// Convert UTXO entries -> Vec<OutPoint> ("txid:vout")
fn utxo_to_outpoints(
    utxos: Vec<(String, UTXO)>
) -> std::result::Result<Vec<String>, String> {
    utxos
        .into_iter()
        .map(|(txid, utxo)| {
            let txid_str = txid
                .split('@')
                .next()
                .ok_or_else(|| format!("Invalid txid format: {txid}"))?;

            if txid_str.is_empty() {
                return Err(format!("Empty txid after parsing: {txid}"));
            }

            Ok(format!("{}:{}", txid_str, utxo.vout))
        })
        .collect()
}

/// Simplified DCR fee model (atoms/kB)
pub fn get_gas_fee(num_input: u64, num_output: u64, fee_rate: u64) -> u64 {
    let tx_size = 12 + num_input * 68 + num_output * 31;
    (fee_rate * tx_size / 1024) + 141
}

/// Choose UTXOs to fund `amount`
pub fn choose_utxos(
    amount: u128,
    utxos: HashMap<String, UTXO>,
    fee_rate: u64,
) -> Result<(Vec<OutPoint>, u128, u128), String> {
    let mut list: Vec<(String, UTXO)> = utxos.into_iter().collect();

    // Pick largest UTXOs first
    list.sort_by(|a, b| b.1.balance.cmp(&a.1.balance));

    let mut selected = Vec::new();
    let mut total: u128 = 0;

    for item in list {
        total += item.1.balance as u128;
        selected.push(item);

        if total >= amount {
            break;
        }
    }

    if total < amount {
        return Err("Insufficient UTXO balance".into());
    }

    let gas_fee = get_gas_fee(
        selected.len() as u64,
        2,
        fee_rate,
    ) as u128;

    let outpoints = utxo_to_outpoints(selected)?;
    Ok((outpoints, total, gas_fee))
}

/// Build DCR Outputs (single recipient + optional change)
pub fn build_dcr_tx_outs(
    recipient_script: String,
    amount: u64,
    change: Option<(String, u64)>,
) -> Vec<DcrTxOut> {
    let mut outs = vec![DcrTxOut {
        value: amount,
        version: 0,
        pk_script: recipient_script,
    }];

    if let Some((script, value)) = change {
        outs.push(DcrTxOut {
            value,
            version: 0,
            pk_script: script,
        });
    }

    outs
}

/// Build many small outputs
pub fn build_dcr_tx_outs_for_management(
    change_address: &str,
    output_count: u64,
    total_value: u64,
    network: Network,
) -> std::result::Result<Vec<DcrTxOut>, String> {
    if output_count == 0 {
        return Err("output_count must be > 0".to_string());
    }

    let change_script = DcrAddress::parse(change_address, network)
        .map_err(|e| format!("Invalid DCR change address '{change_address}': {e}"))?
        .script_pubkey()
        .map_err(|e| format!("Failed to get DCR script_pubkey: {e}"))?;

    let one_amount = total_value / output_count;

    let mut outs = vec![DcrTxOut {
        value: total_value - one_amount * (output_count - 1),
        version: 0,
        pk_script: change_script.clone(),
    }];

    for _ in 0..(output_count - 1) {
        outs.push(DcrTxOut {
            value: one_amount,
            version: 0,
            pk_script: change_script.clone(),
        });
    }

    Ok(outs)
}

/// Active Management — Full DCR version of UTXO consolidation/splitting
pub fn choose_utxos_for_active_management(
    utxos: HashMap<String, UTXO>,
    fee_rate: u64,
    change_address: &str,
    active_limit: (usize, usize),
    max_inputs: usize,
    max_outputs: usize,
    min_amount: usize,
    network: Network,
) -> std::result::Result<(Vec<String>, Vec<DcrTxOut>), String> {
    let mut list: Vec<(String, UTXO)> = utxos.into_iter().collect();
    list.sort_by(|a, b| a.1.balance.cmp(&b.1.balance)); // smallest first

    let mut selected = Vec::new();
    let mut total: u64 = 0;

    // parse change script ONCE
    let change_script = DcrAddress::parse(change_address, network)
        .map_err(|e| format!("Invalid DCR change address '{change_address}': {e}"))?
        .script_pubkey()
        .map_err(|e| format!("Failed to get DCR script_pubkey: {e}"))?;

    // ---------------------------
    // Case 1: Too few UTXOs → Split
    // ---------------------------
    if list.len() < active_limit.0 {
        let use_count = 1;

        for i in 0..use_count {
            total += list[list.len() - 1 - i].1.balance;
            selected.push(list[list.len() - 1 - i].clone());
        }

        let output_count = std::cmp::min(
            active_limit.0 - list.len(),
            std::cmp::min(
                (total as usize / min_amount).saturating_sub(1),
                max_outputs,
            ),
        ) as u64;

        if output_count == 0 {
            return Err("Calculated output_count is zero".to_string());
        }

        let gas_fee = get_gas_fee(1, output_count, fee_rate);

        let outpoints = utxo_to_outpoints(selected)?;
        let outs = build_dcr_tx_outs_for_management(
            change_address,
            output_count,
            total - gas_fee,
            network,
        )?;

        return Ok((outpoints, outs));
    }

    // ---------------------------
    // Case 2: Too many UTXOs → Consolidate
    // ---------------------------
    if list.len() > active_limit.1 {
        let use_count = std::cmp::min(list.len() - active_limit.1, max_inputs);

        for (txid, utxo) in list.iter().take(use_count) {
            total += utxo.balance;
            selected.push((txid.clone(), utxo.clone()));
        }

        let gas_fee = get_gas_fee(use_count as u64, 1, fee_rate);

        let outpoints = utxo_to_outpoints(selected)?;
        let outs = build_dcr_tx_outs(
            change_script,
            total - gas_fee,
            None,
        );

        return Ok((outpoints, outs));
    }

    Err("Incorrect number of UTXOs for active management".to_string())
}