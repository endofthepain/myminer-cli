use crate::Miner;
use ore_api::consts::BUS_ADDRESSES;
use reqwest::Client;
use serde_json::{json, Value};
use url::Url;

// Define the FeeStrategy enum
enum FeeStrategy {
    Helius,
    Triton,
}

impl Miner {
    pub async fn dynamic_fee(&self) -> Option<u64> {
        let rpc_url = self
            .dynamic_fee_url
            .clone()
            .unwrap_or(self.rpc_client.url());

        let host = Url::parse(&rpc_url)
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        
        let strategy = if host.contains("helius-rpc.com") {
            FeeStrategy::Helius
        } else if host.contains("rpcpool.com") {
            FeeStrategy::Triton
        } else {
            return None;
        };

        let client = Client::new();
        let ore_addresses: Vec<String> = std::iter::once(ore_api::ID.to_string())
            .chain(BUS_ADDRESSES.iter().map(|pubkey| pubkey.to_string()))
            .collect();
        
        let body = match strategy {
            FeeStrategy::Helius => {
                json!({
                    "jsonrpc": "2.0",
                    "id": "priority-fee-estimate",
                    "method": "getPriorityFeeEstimate",
                    "params": [{
                        "accountKeys": ore_addresses,
                        "options": {
                            "recommended": true
                        }
                    }]
                })
            }
            FeeStrategy::Triton => {
                json!({
                    "jsonrpc": "2.0",
                    "id": "priority-fee-estimate",
                    "method": "getRecentPrioritizationFees",
                    "params": [
                        ore_addresses,
                        {
                            "percentile": 5000,
                        }
                    ]
                })
            }
        };

        let response: Value = client
            .post(rpc_url)
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        let calculated_fee = match strategy {
            FeeStrategy::Helius => response["result"]["priorityFeeEstimate"]
                .as_f64()
                .map(|fee| fee as u64)
                .ok_or_else(|| format!("Failed to parse priority fee. Response: {:?}", response))
                .unwrap(),
            FeeStrategy::Triton => response["result"]
                .as_array()
                .and_then(|arr| arr.last())
                .and_then(|last| last["prioritizationFee"].as_u64())
                .ok_or_else(|| format!("Failed to parse priority fee. Response: {:?}", response))
                .unwrap(),
        };

        if let Some(max_fee) = self.dynamic_fee_max {
            Some(calculated_fee.min(max_fee))
        } else {
            Some(calculated_fee)
        }
    }
}
