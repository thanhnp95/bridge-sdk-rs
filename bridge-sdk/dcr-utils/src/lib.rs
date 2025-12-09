pub mod address;

use serde_with::{serde_as, DisplayFromStr};
use std::collections::HashMap;

// ---- IMPORT TYPES FROM omni-types ----
use omni_types::dcr::{DcrTxOut, OutPoint};

use crate::address::{DcrAddress, Network};

/// ---- BASIC ERROR TYPE (NO bridge-connector-common) ----
pub type Result<T> = std::result::Result<T, String>;

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
fn utxo_to_outpoints(utxos: Vec<(String, UTXO)>) -> Result<Vec<OutPoint>> {
    utxos
        .into_iter()
        .map(|(txid, utxo)| {
            let txid_str = txid
                .split('@')
                .next()
                .ok_or_else(|| format!("Invalid txid format: {txid}"))?;

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
) -> Result<(Vec<OutPoint>, u128, u128)> {
    let mut list: Vec<(String, UTXO)> = utxos.into_iter().collect();

    // Pick largest UTXOs first
    list.sort_by(|a, b| b.1.balance.cmp(&a.1.balance));

    let mut selected = Vec::new();
    let mut total: u128 = 0;
    let mut gas_fee: u128 = 0;

    for item in list {
        gas_fee = get_gas_fee(selected.len() as u64, 2, fee_rate) as u128;

        if total >= amount + gas_fee {
            break;
        }

        total += item.1.balance as u128;
        selected.push(item);
    }

    if total < amount + gas_fee {
        return Err("Insufficient UTXO balance".into());
    }

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
    change_script: String,
    output_count: u64,
    total_value: u64,
) -> Result<Vec<DcrTxOut>> {
    if output_count == 0 {
        return Err("output_count must be > 0".into());
    }

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
) -> Result<(Vec<OutPoint>, Vec<DcrTxOut>)> {
    let mut list: Vec<(String, UTXO)> = utxos.into_iter().collect();

    list.sort_by(|a, b| a.1.balance.cmp(&b.1.balance)); // smallest first

    let mut selected = Vec::new();
    let mut total: u64 = 0;

    // ---------------------------
    // Case 1: Too few UTXOs → Split
    // ---------------------------
    if list.len() < active_limit.0 {
        let use_count = 1;

        for i in 0..use_count {
            total += list[list.len() - 1 - i].1.balance;
            selected.push(list[i].clone());
        }

        let possible_outputs = std::cmp::min(
            active_limit.0 - list.len(),
            std::cmp::min((total as usize / min_amount).saturating_sub(1), max_outputs),
        );

        let out_count = possible_outputs as u64;

        let gas = get_gas_fee(1, out_count, fee_rate);

        let outpoints = utxo_to_outpoints(selected)?;
        let script = DcrAddress::parse(change_address, network)?
            .script_pubkey()
            .map_err(|e| format!("Script error: {e}"))?;

        let outs = build_dcr_tx_outs_for_management(script, out_count, total - gas)?;

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

        let gas = get_gas_fee(use_count as u64, 1, fee_rate);

        let outpoints = utxo_to_outpoints(selected)?;
        let script = DcrAddress::parse(change_address, network)?
            .script_pubkey()
            .map_err(|e| format!("Script error: {e}"))?;

        let outs = build_dcr_tx_outs(script, total - gas, None);

        return Ok((outpoints, outs));
    }

    Err("Incorrect number of UTXOs for active management".into())
}
