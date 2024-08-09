use std::{sync::Arc, sync::RwLock, time::Instant};
use colored::*;
use drillx::{
    equix::{self},
    Hash, Solution,
};
use ore_api::{
    consts::{BUS_ADDRESSES, BUS_COUNT, EPOCH_DURATION},
    state::{Bus, Config, Proof},
};
use ore_utils::AccountDeserialize;
use rand::Rng;
use solana_program::pubkey::Pubkey;
use solana_rpc_client::spinner;
use solana_sdk::signer::Signer;
use num_cpus;

use crate::{
    args::MineArgs,
    send_and_confirm::ComputeBudget,
    utils::{
        get_clock, get_config, get_updated_proof_with_authority, proof_pubkey,
    },
    Miner,
};

impl Miner {
    pub async fn mine(&self, args: MineArgs) {
        // Register, if needed.
        let signer = self.signer();
        self.open().await;

        // Check number of cores
        self.check_num_cores(args.cores);

        // Initialize variables
        let mut last_hash_at = 0;
        let mut last_balance = 0;
        
        // Start mining loop
        loop {
            // Fetch proof
            let config = get_config(&self.rpc_client).await;
            let proof =
                get_updated_proof_with_authority(&self.rpc_client, signer.pubkey(), last_hash_at)
                    .await;
            last_hash_at = proof.last_hash_at;

            // Fetch SOL balance
            let sol_balance = match self.fetch_sol_balance().await {
                Ok(balance) => balance,
                Err(_) => {
                    println!("Failed to fetch SOL balance.");
                    return;
                }
            };

            // Apply dynamic fee logic
            let priority_fee = if self.dynamic_fee {
                // Calculate dynamic fee, ensuring it doesn't exceed dynamic_fee_max
                let dynamic_fee = self.calculate_dynamic_fee().await;
                if let Some(max_fee) = self.dynamic_fee_max {
                    dynamic_fee.min(max_fee)
                } else {
                    dynamic_fee
                }
            } else {
                self.priority_fee.unwrap_or(0)
            };

            // Calculate the total compute budget, including the priority fee
            let compute_budget = 500_000 + priority_fee;
            let mut ixs = vec![ore_api::instruction::auth(proof_pubkey(signer.pubkey()))];
            if self.should_reset(config).await && rand::thread_rng().gen_range(0..100) == 0 {
                ixs.push(ore_api::instruction::reset(signer.pubkey()));
            }

            ixs.push(ore_api::instruction::mine(
                signer.pubkey(),
                signer.pubkey(),
                self.find_bus().await,
                Self::find_hash_par(
                    proof,
                    self.get_cutoff(proof, args.buffer_time).await,
                    args.cores,
                    args.min_difficulty // Ensure min_difficulty is used here
                ).await,
            ));

            // Submit transaction
            let _result = self.send_and_confirm(
                &ixs,
                ComputeBudget::Fixed(compute_budget.try_into().unwrap()),
                false,
            )
            .await;

            // Display information
            println!(
                "\nSOL Balance: {}\nORE Stake: {}\nChange: {}\nMultiplier: {}",
                format!("{:.9} SOL", sol_balance as f64 / 1_000_000_000.0).white().bold(),
                format!("{:.11} ORE", proof.balance as f64 / 1_000_000_000.0).yellow(),
                format!("{:+.11} ORE", proof.balance.saturating_sub(last_balance) as f64 / 1_000_000_000.0).green(),
                format!("{:.15}", Self::calculate_multiplier(proof.balance, config.top_balance)).blue(),            
            );

            last_balance = proof.balance;
        }
    }

    async fn fetch_sol_balance(&self) -> Result<u64, solana_client::client_error::ClientError> {
        let pubkey = self.signer().pubkey();
        let balance = self.rpc_client.get_balance(&pubkey).await?;
        Ok(balance)
    }

    async fn calculate_dynamic_fee(&self) -> u64 {
        let response = reqwest::get(self.dynamic_fee_url.as_ref().unwrap())
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        response.parse::<u64>().unwrap_or(0)
    }

    async fn find_hash_par(
        proof: Proof,
        cutoff_time: u64,
        cores: u64,
        min_difficulty: u32,
    ) -> Solution {
        // Dispatch job to each thread
        let progress_bar = Arc::new(spinner::new_progress_bar());
        progress_bar.set_message("Mining...");
        let core_ids = core_affinity::get_core_ids().unwrap_or_else(|| {
            panic!("Failed to get core IDs");
        });
        let global_best_difficulty = Arc::new(RwLock::new(0u32));
        let handles: Vec<_> = core_ids
            .into_iter()
            .map(|i| {
                let global_best_difficulty = Arc::clone(&global_best_difficulty);
                std::thread::spawn({
                    let proof = proof.clone();
                    let progress_bar = progress_bar.clone();
                    let mut memory = equix::SolverMemory::new();
                    move || {
                        if (i.id as u64) >= cores {
                            return (0, 0, Hash::default());
                        }

                        let _ = core_affinity::set_for_current(i);

                        let timer = Instant::now();
                        let mut nonce = u64::MAX.saturating_div(cores).saturating_mul(i.id as u64);
                        let mut best_nonce = nonce;
                        let mut best_difficulty = 0;
                        let mut best_hash = Hash::default();
                        loop {
                            if let Ok(hx) = drillx::hash_with_memory(
                                &mut memory,
                                &proof.challenge,
                                &nonce.to_le_bytes(),
                            ) {
                                let difficulty = hx.difficulty();
                                if difficulty > best_difficulty {
                                    best_nonce = nonce;
                                    best_difficulty = difficulty;
                                    best_hash = hx;
                                    let mut global_best = global_best_difficulty.write().unwrap();
                                    if best_difficulty > *global_best {
                                        *global_best = best_difficulty;
                                    }
                                }
                            }

                            if nonce % 100 == 0 {
                                let global_best_difficulty = *global_best_difficulty.read().unwrap();
                                if timer.elapsed().as_secs() >= cutoff_time {
                                    if i.id == 0 {
                                        progress_bar.set_message(format!(
                                            "Mining... ({} / {} difficulty)",
                                            global_best_difficulty,
                                            min_difficulty,
                                        ));
                                    }    
                                    if global_best_difficulty >= min_difficulty {
                                        break;
                                    }
                                } else if i.id == 0 {
                                    progress_bar.set_message(format!(
                                        "Mining... ({} / {} difficulty, {} sec remaining)",
                                        global_best_difficulty,
                                        min_difficulty,
                                        cutoff_time.saturating_sub(timer.elapsed().as_secs()),
                                    ));
                                }
                            }

                            nonce += 1;
                        }

                        (best_nonce, best_difficulty, best_hash)
                    }
                })
            })
            .collect();

        let mut best_nonce = 0;
        let mut best_difficulty = 0;
        let mut best_hash = Hash::default();
        for h in handles {
            if let Ok((nonce, difficulty, hash)) = h.join() {
                if difficulty > best_difficulty {
                    best_difficulty = difficulty;
                    best_nonce = nonce;
                    best_hash = hash;
                }
            }
        }

        progress_bar.finish_with_message(format!(
            "Best hash: {} (difficulty: {})",
            bs58::encode(best_hash.h).into_string(),
            best_difficulty
        ));

        Solution::new(best_hash.d, best_nonce.to_le_bytes())
    }

    pub fn check_num_cores(&self, cores: u64) {
        let num_cores = num_cpus::get() as u64;
        if cores > num_cores {
            println!(
                "{} Number of threads ({}) exceeds available cores ({})",
                "WARNING".bold().yellow(),
                cores,
                num_cores
            );
        }
    }

    async fn should_reset(&self, config: Config) -> bool {
        let clock = get_clock(&self.rpc_client).await;
        config
            .last_reset_at
            .saturating_add(EPOCH_DURATION)
            .saturating_sub(5) // Buffer
            .le(&clock.unix_timestamp)
    }

    async fn get_cutoff(&self, proof: Proof, buffer_time: u64) -> u64 {
        let clock = get_clock(&self.rpc_client).await;
        proof
            .last_hash_at
            .saturating_add(60)
            .saturating_sub(buffer_time as i64)
            .saturating_sub(clock.unix_timestamp)
            .max(0) as u64
    }

    async fn find_bus(&self) -> Pubkey {
        // Fetch the bus with the largest balance
        if let Ok(accounts) = self.rpc_client.get_multiple_accounts(&BUS_ADDRESSES).await {
            let mut top_bus_balance: u64 = 0;
            let mut top_bus = BUS_ADDRESSES[0];
            for account in accounts {
                if let Some(account) = account {
                    if let Ok(bus) = Bus::try_from_bytes(&account.data) {
                        if bus.rewards.gt(&top_bus_balance) {
                            top_bus_balance = bus.rewards;
                            top_bus = BUS_ADDRESSES[bus.id as usize];
                        }
                    }
                }
            }
            return top_bus;
        }

        // Otherwise return a random bus
        let i = rand::thread_rng().gen_range(0..BUS_COUNT);
        BUS_ADDRESSES[i]
    }

    fn calculate_multiplier(balance: u64, top_balance: u64) -> f64 {
        1.0 + (balance as f64 / top_balance as f64).min(1.0f64)
    }
}