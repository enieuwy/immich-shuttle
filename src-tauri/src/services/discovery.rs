use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::services::immich_client::probe_is_immich;

/// Ports Immich commonly listens on, paired with the scheme to probe each with,
/// ordered by preference (the default 2283 wins when a host answers on several).
const CANDIDATE_PORTS: &[(u16, &str)] = &[(2283, "http"), (443, "https"), (80, "http")];

/// A closed port on a live host rejects instantly; this bound only caps the wait
/// for silent/absent hosts so the whole /24 sweep stays snappy.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);

/// Per-candidate cap on the Immich ping once a port is open, so one stalled HTTP
/// endpoint can't burn `probe_is_immich`'s multi-candidate 2s-each budget.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1200);

/// Overall wall-clock ceiling: stop launching new batches past this so a subnet
/// full of slow open ports still returns within a UI-sized window.
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(10);

/// How many candidates to probe at once. Bounds sockets/CPU without needing the
/// tokio `sync` feature: each batch is awaited before the next starts.
const SCAN_CONCURRENCY: usize = 64;

/// Private IPv4 addresses on broadcast-capable, non-loopback interfaces. The
/// broadcast filter excludes point-to-point/VPN interfaces (utun/ppp), so a
/// full-tunnel VPN can't redirect the sweep onto a corporate /24 — we scan the
/// physical LAN(s) the machine is actually attached to.
fn local_lan_ipv4s() -> Vec<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .into_iter()
        .flatten()
        .filter_map(|iface| match iface.addr {
            if_addrs::IfAddr::V4(v4)
                if v4.broadcast.is_some() && v4.ip.is_private() && !v4.ip.is_loopback() =>
            {
                Some(v4.ip)
            }
            _ => None,
        })
        .collect()
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

/// Scan the local LAN(s) for reachable Immich servers, returning confirmed base
/// URLs (deduped, at most one per host — the preferred responding port wins).
/// Read-only: only unauthenticated `/server/ping` probes, never the API key.
/// Bounded by `DISCOVERY_DEADLINE` so it always returns within a UI-sized window.
pub async fn discover_immich_servers() -> Vec<String> {
    // Sweep the /24 of every broadcast-capable private interface, de-duplicated
    // (two addresses on the same subnet, or none at all when offline).
    let mut seen_subnets: HashSet<(u8, u8, u8)> = HashSet::new();
    let mut targets: Vec<(SocketAddr, String)> = Vec::new();
    for local in local_lan_ipv4s() {
        let [a, b, c, _] = local.octets();
        if seen_subnets.insert((a, b, c)) {
            targets.extend(subnet_hosts(local).into_iter().flat_map(host_targets));
        }
    }
    if targets.is_empty() {
        return Vec::new();
    }

    let deadline = Instant::now() + DISCOVERY_DEADLINE;
    let mut found_hosts: HashSet<IpAddr> = HashSet::new();
    let mut confirmed: Vec<String> = Vec::new();
    for chunk in targets.chunks(SCAN_CONCURRENCY) {
        if Instant::now() >= deadline {
            break;
        }
        let mut handles = Vec::with_capacity(chunk.len());
        for (addr, url) in chunk {
            let addr = *addr;
            let url = url.clone();
            handles.push(tokio::spawn(async move {
                if tcp_open(addr).await
                    && matches!(
                        timeout(PROBE_TIMEOUT, probe_is_immich(&url)).await,
                        Ok(true)
                    )
                {
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
