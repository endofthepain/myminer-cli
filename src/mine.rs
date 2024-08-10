use std::{
    sync::{Arc, mpsc::channel},
    sync::atomic::{AtomicU64, AtomicU32, Ordering},
    time::Instant,
    thread,
};

use colored::*;
use drillx::{self, equix, Hash, Solution};
use ore_api::{
    consts::{BUS_ADDRESSES, BUS_COUNT, EPOCH_DURATION}, // Import constants
    state::{Bus, Config, Proof},
};
use ore_utils::AccountDeserialize;
use rand::Rng;
use solana_program::pubkey::Pubkey;
use solana_rpc_client::spinner;
use solana_sdk::signer::Signer;

use crate::{
    args::MineArgs,
    send_and_confirm::ComputeBudget,
    utils::{
        amount_u64_to_string, get_clock, get_config, get_updated_proof_with_authority, proof_pubkey,
    },
    Miner,
};

impl Miner {
    pub async fn mine(&self, args: MineArgs) {
        let signer = self.signer();
        self.open().await;

        self.check_num_cores(args.cores);

        let mut last_hash_at = 0;
        let mut last_balance = 0;

        loop {
            let config = get_config(&self.rpc_client).await;
            let proof = get_updated_proof_with_authority(&self.rpc_client, signer.pubkey(), last_hash_at)
                .await;
            last_hash_at = proof.last_hash_at;

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

            let cutoff_time = self.get_cutoff(proof, args.buffer_time).await;
            let (solution, hash_rate) = Self::find_hash_par(
                proof,
                cutoff_time,
                args.cores,
                config.min_difficulty as u32 // Ensure min_difficulty is used here
            ).await;

            ixs.push(ore_api::instruction::mine(
                signer.pubkey(),
                signer.pubkey(),
                self.find_bus().await,
                solution,
            ));

            // Submit transaction
            let _result = self.send_and_confirm(
                &ixs,
                ComputeBudget::Fixed(compute_budget.try_into().unwrap()),
                false,
            )
            .await;

            // Fetch SOL balance
            let sol_balance = match self.fetch_sol_balance().await {
                Ok(balance) => balance,
                Err(_) => {
                    println!("Failed to fetch SOL balance.");
                    return;
                }
            };

            // Print mining status
            println!(
                "\n{}: {:.9} SOL\n{}: {}\n{}: {}",
                "SOL BALANCE".white().bold(),
                sol_balance as f64 / 1_000_000_000.0, // Convert lamports to SOL
                "ORE Stake".white().bold(),
                amount_u64_to_string(proof.balance).yellow(),
                "Change".white().bold(),
                amount_u64_to_string(proof.balance.saturating_sub(last_balance)).green()
            );
            println!(
                "{}: {:12}x\n{}: {:.2} H/S",
                "Multiplier".white().bold(),
                format!("{:12}", Self::calculate_multiplier(proof.balance, config.top_balance)).blue(),
                "Hash".white().bold(),
                hash_rate as f64
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
        mut cutoff_time: u64,
        cores: u64,
        min_difficulty: u32,
    ) -> (Solution, u64) {
        let (tx, rx) = channel();
        let progress_bar = Arc::new(spinner::new_progress_bar());
        let global_best_difficulty = Arc::new(AtomicU32::new(0));
        let global_total_hashes = Arc::new(AtomicU64::new(0));
        progress_bar.set_message("Mining...");
    
        let start_time = Instant::now();
    
        for i in 0..cores {
            let tx = tx.clone();
            let global_best_difficulty = Arc::clone(&global_best_difficulty);
            let global_total_hashes = Arc::clone(&global_total_hashes);
            let proof = proof.clone();
            let progress_bar = progress_bar.clone();
    
            thread::spawn(move || {
                let mut memory = equix::SolverMemory::new();
                let timer = Instant::now();
                let mut nonce = u64::MAX.saturating_div(cores).saturating_mul(i);
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
    
                            let prev_best_difficulty = global_best_difficulty.fetch_max(best_difficulty, Ordering::Relaxed);
                            if best_difficulty > prev_best_difficulty {
                                cutoff_time += 10;
                            }
                        }
                    }
    
                    global_total_hashes.fetch_add(1, Ordering::Relaxed);
    
                    if nonce % 100 == 0 {
                        let global_best_difficulty = global_best_difficulty.load(Ordering::Relaxed);
                        let total_hashes = global_total_hashes.load(Ordering::Relaxed);
                        let elapsed_time = start_time.elapsed().as_secs_f64();
                        let hash_rate = total_hashes as f64 / elapsed_time;
    
                        if timer.elapsed().as_secs() >= cutoff_time {
                            if i == 0 {
                                progress_bar.set_message(format!(
                                    "Mining... (difficulty {}, time {}, {:.2} H/s)",
                                    global_best_difficulty,
                                    Self::format_duration(
                                        (cutoff_time
                                            .saturating_sub(timer.elapsed().as_secs())) as u32
                                    ),
                                    hash_rate,
                                ));
                            }
                            if global_best_difficulty >= min_difficulty {
                                break;
                            }
                        } else if i == 0 {
                            progress_bar.set_message(format!(
                                "Mining... (difficulty {}, time {}, {:.2} H/s)",
                                global_best_difficulty,
                                Self::format_duration(
                                    (cutoff_time.saturating_sub(timer.elapsed().as_secs())) as u32
                                ),
                                hash_rate,
                            ));
                        }
                    }
    
                    nonce += cores;
                }
    
                tx.send((best_nonce.to_le_bytes(), best_difficulty, best_hash)).unwrap();
            });
        }
    
        let mut best_nonce = [0; 8]; // Adjust according to Solution definition
        let mut best_difficulty = 0;
        let mut best_hash = Hash::default();
        for _ in 0..cores {
            let (nonce_bytes, difficulty, hash) = rx.recv().unwrap();
            if difficulty > best_difficulty {
                best_difficulty = difficulty;
                best_nonce = nonce_bytes;
                best_hash = hash;
            }
        }
    
        (Solution { n: best_nonce, d: best_difficulty }, global_total_hashes.load(Ordering::Relaxed))
    }

    fn check_num_cores(&self, cores: u64) {
        let num_cores = num_cpus::get() as u64;
        if cores > num_cores {
            panic!("Number of cores specified exceeds available cores.");
        }
    }

    async fn should_reset(&self, config: Config) -> bool {
        let epoch_time = get_clock(&self.rpc_client).await as u64;
        let epoch_duration = EPOCH_DURATION as u64;
        let current_epoch = epoch_time / epoch_duration;
        let last_reset = config.last_reset_at as u64 * epoch_duration;
        let last_reset_epoch = last_reset / epoch_duration;

        let reset_period = 10; // Replace with actual reset period value
        let should_reset = current_epoch - last_reset_epoch >= reset_period;
        should_reset
    }

    async fn get_cutoff(&self, proof: Proof, buffer_time: u64) -> u64 {
        let clock = get_clock(&self.rpc_client).await as u64;
        let epoch_duration = EPOCH_DURATION as u64;
        let current_epoch = clock / epoch_duration;
        let proof_epoch = proof.last_hash_at as u64 / epoch_duration;
        let epoch_diff = current_epoch - proof_epoch;

        let mut cutoff_time = proof.last_hash_at as u64 + (epoch_diff * epoch_duration) + buffer_time;
        cutoff_time
    }

    fn format_duration(seconds: u32) -> String {
        let minutes = seconds / 60;
        let remaining_seconds = seconds % 60;
        format!("{:02}:{:02}", minutes, remaining_seconds)
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
        1.0 + (balance as f64 / top_balance as f64).min(1.0)
    }
}
