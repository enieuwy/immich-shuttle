use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::services::immich_client::probe_is_immich;

/// Ports Immich commonly listens on, paired with the scheme to probe each with,
/// ordered by preference (the default 2283 wins when a host answers on several).
const CANDIDATE_PORTS: &[(u16, &str)] = &[(2283, "http"), (443, "https"), (80, "http")];

/// A closed port on a live host rejects instantly; this bound only caps the wait
/// for silent/absent hosts so the whole /24 sweep stays snappy.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);

/// How many candidates to probe at once. Bounds sockets/CPU without needing the
/// tokio `sync` feature: each batch is awaited before the next starts.
const SCAN_CONCURRENCY: usize = 64;

/// Best-effort local IPv4 of the primary interface. Opens a UDP socket and
/// "connects" it to a public address to learn which local IP the OS would route
/// from; no packets are actually sent.
fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

/// Every host address in the local /24 except the network address, the broadcast
/// address, and the machine's own address.
fn subnet_hosts(local: Ipv4Addr) -> Vec<Ipv4Addr> {
    let [a, b, c, _] = local.octets();
    (1u8..=254)
        .map(|d| Ipv4Addr::new(a, b, c, d))
        .filter(|ip| *ip != local)
        .collect()
}

/// (socket addr, base URL) probe targets for one host across candidate ports.
fn host_targets(ip: Ipv4Addr) -> Vec<(SocketAddr, String)> {
    CANDIDATE_PORTS
        .iter()
        .map(|(port, scheme)| {
            let addr = SocketAddr::from((ip, *port));
            let url = match (*scheme, *port) {
                ("https", 443) => format!("https://{ip}"),
                ("http", 80) => format!("http://{ip}"),
                (scheme, port) => format!("{scheme}://{ip}:{port}"),
            };
            (addr, url)
        })
        .collect()
}

async fn tcp_open(addr: SocketAddr) -> bool {
    matches!(
        timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

/// Scan the local /24 for reachable Immich servers, returning confirmed base
/// URLs (deduped, at most one per host — the preferred responding port wins).
/// Read-only: only unauthenticated `/server/ping` probes, never the API key.
pub async fn discover_immich_servers() -> Vec<String> {
    let Some(local) = local_ipv4() else {
        return Vec::new();
    };
    let targets: Vec<(SocketAddr, String)> = subnet_hosts(local)
        .into_iter()
        .flat_map(host_targets)
        .collect();

    let mut found_hosts: HashSet<IpAddr> = HashSet::new();
    let mut confirmed: Vec<String> = Vec::new();
    for chunk in targets.chunks(SCAN_CONCURRENCY) {
        let mut handles = Vec::with_capacity(chunk.len());
        for (addr, url) in chunk {
            let addr = *addr;
            let url = url.clone();
            handles.push(tokio::spawn(async move {
                if tcp_open(addr).await && probe_is_immich(&url).await {
                    Some((addr.ip(), url))
                } else {
                    None
                }
            }));
        }
        // Await in submission order so the preferred port (listed first per host)
        // is the one that claims the host in the dedupe set.
        for handle in handles {
            if let Ok(Some((ip, url))) = handle.await {
                if found_hosts.insert(ip) {
                    confirmed.push(url);
                }
            }
        }
    }
    confirmed
}

#[cfg(test)]
mod tests {
    use super::{host_targets, subnet_hosts};
    use std::net::Ipv4Addr;

    #[test]
    fn subnet_hosts_covers_the_24_excluding_self_network_and_broadcast() {
        let local = Ipv4Addr::new(192, 168, 1, 50);
        let hosts = subnet_hosts(local);
        // .1..=.254 is 254 addresses, minus the machine's own -> 253.
        assert_eq!(hosts.len(), 253);
        assert!(!hosts.contains(&local));
        assert!(!hosts.contains(&Ipv4Addr::new(192, 168, 1, 0)));
        assert!(!hosts.contains(&Ipv4Addr::new(192, 168, 1, 255)));
        assert!(hosts.contains(&Ipv4Addr::new(192, 168, 1, 1)));
        assert!(hosts.contains(&Ipv4Addr::new(192, 168, 1, 254)));
    }

    #[test]
    fn host_targets_builds_scheme_correct_urls() {
        let ip = Ipv4Addr::new(10, 0, 0, 5);
        let urls: Vec<String> = host_targets(ip).into_iter().map(|(_, url)| url).collect();
        assert_eq!(
            urls,
            vec![
                "http://10.0.0.5:2283".to_string(),
                "https://10.0.0.5".to_string(),
                "http://10.0.0.5".to_string(),
            ]
        );
    }
}
