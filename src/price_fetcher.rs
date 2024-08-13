use reqwest::Client;
use serde_json::Value;
use std::error::Error;

const COINGECKO_API_URL: &str = "https://api.coingecko.com/api/v3/simple/price?ids=solana&vs_currencies=usd";
const DEXSCREENER_API_URL: &str = "https://api.dexscreener.com/latest/dex/pairs/solana/ggadtfbqdgjozz3fp7zrtofgwnrs4e6mczmmd5ni1mxj";

pub async fn get_solana_price_usd() -> Result<f64, Box<dyn Error>> {
    let client = Client::new();
    let response = client
        .get(COINGECKO_API_URL)
        .send()
        .await?
        .json::<Value>()
        .await?;

    let sol_price_usd = response["solana"]["usd"].as_f64().unwrap_or(0.0);
    Ok(sol_price_usd)
}

pub async fn get_ore_price_usd() -> Result<f64, Box<dyn Error>> {
    let client = Client::new();
    let response = client
        .get(DEXSCREENER_API_URL)
        .send()
        .await?
        .json::<Value>()
        .await?;
    
    // Extract priceUsd from the pair object
    let price_usd = response["pair"]["priceUsd"]
        .as_str()
        .unwrap_or("0")
        .parse::<f64>()
        .unwrap_or(0.0);
    
    Ok(price_usd)
}
