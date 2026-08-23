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
/// for silent/absent hosts so a subnet sweep stays snappy.
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

/// Maximum number of host addresses generated for one interface. A /8 can contain
/// more than 16 million hosts, so this cap prevents discovery from allocating and
/// probing an unbounded target list while still covering four /24-sized blocks.
const MAX_HOSTS_PER_INTERFACE: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct LanIpv4 {
    ip: Ipv4Addr,
    netmask: Ipv4Addr,
}

fn network_address(ip: Ipv4Addr, netmask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(ip) & u32::from(netmask))
}

/// Private IPv4 addresses on broadcast-capable, non-loopback interfaces. The
/// broadcast filter excludes point-to-point/VPN interfaces (utun/ppp), so a
/// full-tunnel VPN cannot redirect the sweep onto a corporate network — we scan
/// the physical LAN(s) the machine is actually attached to.
fn local_lan_ipv4s() -> Vec<LanIpv4> {
    if_addrs::get_if_addrs()
        .into_iter()
        .flatten()
        .filter_map(|iface| match iface.addr {
            if_addrs::IfAddr::V4(v4)
                if v4.broadcast.is_some() && v4.ip.is_private() && !v4.ip.is_loopback() =>
            {
                Some(LanIpv4 {
                    ip: v4.ip,
                    netmask: v4.netmask,
                })
            }
            _ => None,
        })
        .collect()
}

/// Keep one interface for each network and netmask, preserving discovery order.
fn deduplicated_lan_ipv4s(interfaces: impl IntoIterator<Item = LanIpv4>) -> Vec<LanIpv4> {
    let mut seen_subnets = HashSet::new();
    interfaces
        .into_iter()
        .filter(|interface| {
            seen_subnets.insert((
                network_address(interface.ip, interface.netmask),
                interface.netmask,
            ))
        })
        .collect()
}

fn host_bounds(local: Ipv4Addr, netmask: Ipv4Addr) -> Option<(u32, u32)> {
    let network = u32::from(network_address(local, netmask));
    let broadcast = network | !u32::from(netmask);
    let first_host = network.checked_add(1)?;
    let last_host = broadcast.checked_sub(1)?;
    (first_host <= last_host).then_some((first_host, last_host))
}

/// Every host address in the interface's network except the network address, the
/// broadcast address, and the machine's own address.
///
/// A network with at most `MAX_HOSTS_PER_INTERFACE` hosts keeps ascending order.
/// For a wider network, the machine's own /24 comes first, then addresses expand
/// outward below and above that /24 in alternating order.
fn subnet_hosts(local: Ipv4Addr, netmask: Ipv4Addr) -> Vec<Ipv4Addr> {
    let Some((first_host, last_host)) = host_bounds(local, netmask) else {
        return Vec::new();
    };
    let host_count = u64::from(last_host) - u64::from(first_host) + 1;
    if host_count <= MAX_HOSTS_PER_INTERFACE as u64 {
        return (first_host..=last_host)
            .map(Ipv4Addr::from)
            .filter(|ip| *ip != local)
            .collect();
    }

    let local_24_network = u32::from(local) & 0xffffff00;
    let local_24_first = local_24_network.max(first_host);
    let local_24_last = (local_24_network | 0xff).min(last_host);
    let mut hosts = Vec::with_capacity(MAX_HOSTS_PER_INTERFACE);
    if local_24_first <= local_24_last {
        for candidate in local_24_first..=local_24_last {
            if candidate != u32::from(local) {
                hosts.push(Ipv4Addr::from(candidate));
            }
        }
    }

    let mut distance = 1u32;
    while hosts.len() < MAX_HOSTS_PER_INTERFACE {
        let mut expanded = false;
        if let Some(candidate) = local_24_first.checked_sub(distance) {
            if candidate >= first_host && candidate <= last_host && candidate != u32::from(local) {
                hosts.push(Ipv4Addr::from(candidate));
                expanded = true;
            }
        }
        if hosts.len() >= MAX_HOSTS_PER_INTERFACE {
            break;
        }
        if let Some(candidate) = local_24_last.checked_add(distance) {
            if candidate >= first_host && candidate <= last_host && candidate != u32::from(local) {
                hosts.push(Ipv4Addr::from(candidate));
                expanded = true;
            }
        }
        if !expanded || distance == u32::MAX {
            break;
        }
        distance += 1;
    }
    hosts
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
    // Sweep the actual network of every broadcast-capable private interface,
    // de-duplicated when multiple addresses share the same network and netmask.
    let mut targets: Vec<(SocketAddr, String)> = Vec::new();
    for interface in deduplicated_lan_ipv4s(local_lan_ipv4s()) {
        targets.extend(
            subnet_hosts(interface.ip, interface.netmask)
                .into_iter()
                .flat_map(host_targets),
        );
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
        // `JoinSet` (not bare `tokio::spawn`) so that dropping this future — the
        // caller closes the profile dialog, switches screens, or kicks off a new
        // scan — aborts every still-running probe in this chunk instead of
        // leaking up to `SCAN_CONCURRENCY` detached TCP connects/HTTP requests
        // that keep running to completion in the background.
        let mut set = tokio::task::JoinSet::new();
        for (index, (addr, url)) in chunk.iter().enumerate() {
            let addr = *addr;
            let url = url.clone();
            set.spawn(async move {
                let found = if tcp_open(addr).await
                    && matches!(
                        timeout(PROBE_TIMEOUT, probe_is_immich(&url)).await,
                        Ok(true)
                    ) {
                    Some((addr.ip(), url))
                } else {
                    None
                };
                (index, found)
            });
        }
        // `JoinSet::join_next` yields in completion order, not submission order,
        // so collect every result first and sort by the submission index before
        // folding into `found_hosts` — the preferred port (listed first per host
        // by `host_targets`) must be the one that claims the host in the dedupe
        // set, regardless of which probe happens to finish first.
        let mut results = Vec::with_capacity(chunk.len());
        while let Some(joined) = set.join_next().await {
            if let Ok(result) = joined {
                results.push(result);
            }
        }
        results.sort_by_key(|(index, _)| *index);
        for (_, found) in results {
            if let Some((ip, url)) = found {
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
    use super::{
        deduplicated_lan_ipv4s, host_targets, subnet_hosts, LanIpv4, MAX_HOSTS_PER_INTERFACE,
    };
    use std::net::Ipv4Addr;

    #[test]
    fn subnet_hosts_covers_the_24_in_the_existing_host_order() {
        let local = Ipv4Addr::new(192, 168, 1, 50);
        let netmask = Ipv4Addr::new(255, 255, 255, 0);
        let hosts = subnet_hosts(local, netmask);
        let expected: Vec<_> = (1u8..=254)
            .map(|last_octet| Ipv4Addr::new(192, 168, 1, last_octet))
            .filter(|ip| *ip != local)
            .collect();

        assert_eq!(hosts, expected);
    }

    #[test]
    fn subnet_hosts_caps_a_16_and_reaches_outside_the_local_24() {
        let local = Ipv4Addr::new(10, 42, 7, 50);
        let netmask = Ipv4Addr::new(255, 255, 0, 0);
        let hosts = subnet_hosts(local, netmask);

        assert!(hosts.len() <= MAX_HOSTS_PER_INTERFACE);
        assert!(hosts.iter().any(|ip| ip.octets()[2] != local.octets()[2]));
    }

    #[test]
    fn subnet_hosts_excludes_network_broadcast_and_interface_addresses() {
        let local = Ipv4Addr::new(172, 16, 7, 50);
        let netmask = Ipv4Addr::new(255, 255, 0, 0);
        let hosts = subnet_hosts(local, netmask);

        assert!(!hosts.contains(&Ipv4Addr::new(172, 16, 0, 0)));
        assert!(!hosts.contains(&Ipv4Addr::new(172, 16, 255, 255)));
        assert!(!hosts.contains(&local));
    }

    #[test]
    fn interfaces_on_the_same_network_produce_one_sweep() {
        let first = LanIpv4 {
            ip: Ipv4Addr::new(192, 168, 1, 10),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
        };
        let second = LanIpv4 {
            ip: Ipv4Addr::new(192, 168, 1, 200),
            netmask: first.netmask,
        };

        assert_eq!(deduplicated_lan_ipv4s([first, second]), vec![first]);
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
