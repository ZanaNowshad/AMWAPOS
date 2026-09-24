//! LAN discovery: terminals broadcast a UDP probe; hubs answer with their
//! address. Discovery only helps the operator find the hub; trust is
//! established by the pairing code and signed requests, never by discovery.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket as StdUdp};
use std::sync::Arc;
use std::time::Duration;

use amwapos_core::AppCore;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;

pub const DISCOVERY_PORT: u16 = 47801;
const PROBE: &[u8] = b"AMWAPOS_DISCOVER_V1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Found {
    pub url: String,
    pub hub_name: String,
    pub business_name: String,
    pub version: String,
}

/// Best-effort list of this machine's LAN IPv4 addresses.
pub fn local_addresses() -> Vec<String> {
    let mut out = vec![];
    for probe in ["10.255.255.255:1", "192.168.255.255:1", "172.31.255.255:1"] {
        if let Ok(s) = StdUdp::bind("0.0.0.0:0") {
            if s.connect(probe).is_ok() {
                if let Ok(a) = s.local_addr() {
                    let ip = a.ip().to_string();
                    if ip != "0.0.0.0" && !out.contains(&ip) {
                        out.push(ip);
                    }
                }
            }
        }
    }
    if out.is_empty() {
        out.push("127.0.0.1".into());
    }
    out
}

/// Hub-side responder. Runs until the task is aborted.
pub async fn respond(core: Arc<AppCore>, port: u16) -> std::io::Result<()> {
    let sock = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT))).await?;
    let mut buf = [0u8; 64];
    loop {
        let (n, peer) = sock.recv_from(&mut buf).await?;
        if &buf[..n] != PROBE {
            continue;
        }
        let c = core.clone();
        let info = tokio::task::spawn_blocking(move || c.hub_info()).await;
        if let Ok(Ok(i)) = info {
            let reply = serde_json::json!({ "port": port, "hub_name": i.hub_name, "business_name": i.business_name, "version": i.app_version });
            let _ = sock.send_to(reply.to_string().as_bytes(), peer).await;
        }
    }
}

/// Terminal-side: broadcast a probe and collect answers for `wait`.
pub async fn discover(wait: Duration) -> std::io::Result<Vec<Found>> {
    let sock = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).await?;
    sock.set_broadcast(true)?;
    let _ = sock.send_to(PROBE, SocketAddr::from((Ipv4Addr::BROADCAST, DISCOVERY_PORT))).await;
    let _ = sock.send_to(PROBE, SocketAddr::from((Ipv4Addr::LOCALHOST, DISCOVERY_PORT))).await;
    let mut found: Vec<Found> = vec![];
    let deadline = tokio::time::Instant::now() + wait;
    let mut buf = [0u8; 1024];
    while let Ok(Ok((n, peer))) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) {
            let f = Found {
                url: format!("http://{}:{}", peer.ip(), v["port"].as_u64().unwrap_or(47800)),
                hub_name: v["hub_name"].as_str().unwrap_or("").into(),
                business_name: v["business_name"].as_str().unwrap_or("").into(),
                version: v["version"].as_str().unwrap_or("").into(),
            };
            if !found.contains(&f) {
                found.push(f);
            }
        }
    }
    Ok(found)
}
