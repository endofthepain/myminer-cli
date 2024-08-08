use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

use colored::*;
use solana_client::{
    client_error::{ClientError, ClientErrorKind, Result as ClientResult},
    rpc_config::RpcSendTransactionConfig,
};
use solana_program::{
    instruction::Instruction,
    native_token::{lamports_to_sol, sol_to_lamports},
};
use solana_rpc_client::spinner;
use solana_sdk::{
    commitment_config::CommitmentLevel,
    compute_budget::ComputeBudgetInstruction,
    signature::{Signature, Signer},
    transaction::Transaction,
};
use solana_transaction_status::{TransactionConfirmationStatus, UiTransactionEncoding};
use tokio::time::sleep;

use crate::Miner;

const MIN_SOL_BALANCE: f64 = 0.0005;
const RPC_RETRIES: usize = 0;
const GATEWAY_RETRIES: usize = 150;
const CONFIRM_RETRIES: usize = 8;
const CONFIRM_DELAY: u64 = 0;
const GATEWAY_DELAY: u64 = 300;

pub enum ComputeBudget {
    Dynamic,
    Fixed(u32),
}

impl Miner {
    pub async fn send_and_confirm(
        &self,
        ixs: &[Instruction],
        compute_budget: ComputeBudget,
        skip_confirm: bool,
    ) -> ClientResult<Signature> {
        let signer = self.signer();
        let client = self.rpc_client.clone();
        let fee_payer = self.fee_payer();

        // Check balance
        let balance = client.get_balance(&fee_payer.pubkey()).await?;
        if balance <= sol_to_lamports(MIN_SOL_BALANCE) {
            panic!(
                "{} Insufficient balance: {} SOL\nPlease top up with at least {} SOL",
                "ERROR".bold().red(),
                lamports_to_sol(balance),
                MIN_SOL_BALANCE
            );
        }

        // Set compute units
        let mut final_ixs = vec![];
        match compute_budget {
            ComputeBudget::Dynamic => {
                final_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(1_400_000));
            }
            ComputeBudget::Fixed(cus) => {
                final_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(cus));
            }
        }

        // Set compute unit price
        final_ixs.push(ComputeBudgetInstruction::set_compute_unit_price(
            self.priority_fee.unwrap_or(0),
        ));
        final_ixs.extend_from_slice(ixs);

        // Build tx
        let send_cfg = RpcSendTransactionConfig {
            skip_preflight: true,
            preflight_commitment: Some(CommitmentLevel::Confirmed),
            encoding: Some(UiTransactionEncoding::Base64),
            max_retries: Some(RPC_RETRIES),
            min_context_slot: None,
        };
        let mut tx = Transaction::new_with_payer(&final_ixs, Some(&fee_payer.pubkey()));

        // Create and configure progress bar
        let progress_bar = ProgressBar::new_spinner();
        progress_bar.set_style(
            ProgressStyle::with_template("{spinner:.green} [{elapsed_precise}] {msg}")
                .unwrap()
                .tick_chars("/|\\- "),
        );

        // Submit tx
        let mut attempts = 0;
        loop {
            progress_bar.set_message(format!("Submitting transaction... (attempt {})", attempts));

            // Sign tx with a new blockhash
            if attempts % 5 == 0 {
                if self.dynamic_fee_strategy.is_some() {
                    let fee = self.dynamic_fee().await?;
                    final_ixs[1] = ComputeBudgetInstruction::set_compute_unit_price(fee);
                    progress_bar.println(format!("  Priority fee: {} microlamports", fee));
                }

                let (hash, _slot) = client
                    .get_latest_blockhash_with_commitment(self.rpc_client.commitment())
                    .await?;
                tx.sign(&[&signer, &fee_payer], hash);
            }

            // Send transaction            
            match client.send_transaction_with_config(&tx, send_cfg).await {
                Ok(sig) => {
                    if skip_confirm {
                        progress_bar.finish_with_message(format!("Sent: {}", sig));
                        return Ok(sig);
                    }

                    // Confirm transaction
                    for _ in 0..CONFIRM_RETRIES {
                        tokio::time::sleep(Duration::from_millis(CONFIRM_DELAY)).await;
                        match client.get_signature_statuses(&[sig]).await {
                            Ok(signature_statuses) => {
                                for status in signature_statuses.value {
                                    if let Some(status) = status {
                                        if let Some(err) = status.err {
                                            progress_bar.finish_with_message(format!(
                                                "{}: {}",
                                                "ERROR".bold().red(),
                                                err
                                            ));
                                            return Err(ClientError {
                                                request: None,
                                                kind: ClientErrorKind::Custom(err.to_string()),
                                            });
                                        }
                                        if let Some(confirmation) = status.confirmation_status {
                                            match confirmation {
                                                TransactionConfirmationStatus::Confirmed
                                                | TransactionConfirmationStatus::Finalized => {
                                                    progress_bar.finish_with_message(format!(
                                                        "{} {}",
                                                        "OK".bold().green(),
                                                        sig
                                                    ));
                                                    return Ok(sig);
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                            Err(err) => {
                                progress_bar.set_message(format!(
                                    "{}: {}",
                                    "ERROR".bold().red(),
                                    err.kind().to_string()
                                ));
                            }
                        }
                    }
                }
                Err(err) => {
                    progress_bar.set_message(format!(
                        "{}: {}",
                        "ERROR".bold().red(),
                        err.kind().to_string()
                    ));
                }
            }

            // Retry
            tokio::time::sleep(Duration::from_millis(GATEWAY_DELAY)).await;
            attempts += 1;
            if attempts > GATEWAY_RETRIES {
                progress_bar.finish_with_message(format!("{}: Max retries", "ERROR".bold().red()));
                return Err(ClientError {
                    request: None,
                    kind: ClientErrorKind::Custom("Max retries".into()),
                });
            }
        }
    }

    async fn update_blockhash_and_resign_tx(
        &self,
        client: &solana_client::nonblocking::rpc_client::RpcClient,
        signer: &dyn Signer,
        fee_payer: &dyn Signer,
        tx: &mut Transaction,
        final_ixs: &mut Vec<Instruction>,
    ) -> ClientResult<()> {
        if self.dynamic_fee_strategy.is_some() {
            let fee = self.dynamic_fee().await;
            final_ixs[1] = ComputeBudgetInstruction::set_compute_unit_price(fee);
        }

        let (hash, _slot) = client
            .get_latest_blockhash_with_commitment(client.commitment())
            .await?;
        if signer.pubkey() == fee_payer.pubkey() {
            tx.sign(&[signer], hash);
        } else {
            tx.sign(&[signer, fee_payer], hash);
        }
        Ok(())
    }

    async fn confirm_tx(
        &self,
        client: &solana_client::nonblocking::rpc_client::RpcClient,
        sig: Signature,
        progress_bar: &indicatif::ProgressBar,
    ) -> ClientResult<Signature> {
        for _ in 0..CONFIRM_RETRIES {
            sleep(Duration::from_millis(CONFIRM_DELAY)).await;
            match client.get_signature_statuses(&[sig]).await {
                Ok(signature_statuses) => {
                    for status in signature_statuses.value {
                        if let Some(status) = status {
                            if let Some(err) = status.err {
                                progress_bar.finish_with_message(format!(
                                    "{}: {}",
                                    "ERROR".bold().red(),
                                    err
                                ));
                                return Err(ClientError {
                                    request: None,
                                    kind: ClientErrorKind::Custom(err.to_string()),
                                });
                            }
                            if let Some(confirmation) = status.confirmation_status {
                                match confirmation {
                                    TransactionConfirmationStatus::Confirmed
                                    | TransactionConfirmationStatus::Finalized => {
                                        progress_bar.finish_with_message(format!(
                                            "{} {}",
                                            "OK".bold().green(),
                                            sig
                                        ));
                                        return Ok(sig);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    progress_bar.set_message(format!(
                        "{}: {}",
                        "ERROR".bold().red(),
                        err.kind().to_string()
                    ));
                }
            }
        }
        Err(ClientError {
            request: None,
            kind: ClientErrorKind::Custom("Confirmation failed".into()),
        })
    }
}
