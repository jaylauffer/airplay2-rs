//! Scan for AirPlay devices and print one JSON object per line.
//!
//! Written for `sng-bass-blaster`'s "Broadcast" device dropdown, which
//! spawns this as a subprocess and parses its stdout -- keeps that app's
//! own binary free of a direct dependency on this GPL-2.0 crate.
//!
//! Run with: cargo run -p airplay-discovery --example list_json -- [seconds]

use airplay_discovery::{Discovery, ServiceBrowser};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);

    let browser = ServiceBrowser::new()?;
    let devices = browser.scan(Duration::from_secs(seconds)).await?;

    for device in devices {
        let ip = device
            .addresses
            .iter()
            .find(|addr| addr.is_ipv4())
            .or_else(|| device.addresses.first());
        let Some(ip) = ip else { continue };

        println!(
            "{{\"name\":\"{}\",\"id\":\"{}\",\"ip\":\"{}\",\"port\":{},\"airplay2\":{},\"model\":\"{}\"}}",
            json_escape(&device.name),
            device.id.to_mac_string(),
            ip,
            device.port,
            device.supports_airplay2(),
            json_escape(&device.model),
        );
    }

    Ok(())
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
