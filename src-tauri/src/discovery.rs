use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr},
    time::Instant,
};
use tokio::net::TcpStream;
use tauri::Emitter;

/// One local network adapter address with its real subnet mask.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AdapterInfo {
    /// Friendly adapter name ("Ethernet", "Wi-Fi", "Ethernet 2", Radmin VPN…)
    pub name: String,
    /// Unicast IPv4 address of this adapter
    pub ip: String,
    /// Subnet prefix length (real mask; 24 = unknown/fallback)
    pub prefix: u8,
    /// Adapter is operational (IfOperStatusUp)
    pub up: bool,
}

fn usable_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    // skip loopback / multicast / reserved / link-local 169.254.*
    !(o[0] == 127 || o[0] >= 224 || (o[0] == 169 && o[1] == 254))
}

fn to_u32(v4: Ipv4Addr) -> u32 {
    u32::from_be_bytes(v4.octets())
}
fn from_u32(x: u32) -> Ipv4Addr {
    Ipv4Addr::from(x)
}
fn prefix_mask(prefix: u8) -> u32 {
    if prefix == 0 || prefix > 32 {
        0
    } else if prefix == 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - prefix as u32)
    }
}

/// All active network adapters with real subnet masks (Windows `ipconfig` API).
/// Falls back to `local-ip-address` (mask unknown => /24) if that fails.
pub fn my_adapters() -> Vec<AdapterInfo> {
    let mut out: Vec<AdapterInfo> = Vec::new();

    #[cfg(windows)]
    if let Ok(adapters) = ipconfig::get_adapters() {
        for a in adapters {
            if matches!(a.if_type(), ipconfig::IfType::SoftwareLoopback) {
                continue;
            }
            let up = matches!(a.oper_status(), ipconfig::OperStatus::IfOperStatusUp);
            let name = a.friendly_name().to_string();
            for ip in a.ip_addresses() {
                let IpAddr::V4(v4) = *ip else { continue };
                if !usable_v4(v4) {
                    continue;
                }
                // pick the most specific on-link prefix that contains this address
                let x = to_u32(v4);
                let mut prefix: u8 = 24;
                let mut matched = false;
                for (net, plen) in a.prefixes() {
                    let (IpAddr::V4(n4), plen) = (net, *plen) else { continue };
                    let p = plen.clamp(0, 32) as u8;
                    if p == 0 || p > 30 {
                        continue; // ignore /0 and /31-/32 noise
                    }
                    let m = prefix_mask(p);
                    if (x & m) == to_u32(*n4) && (!matched || p >= prefix) {
                        prefix = p;
                        matched = true;
                    }
                }
                out.push(AdapterInfo { name: name.clone(), ip: v4.to_string(), prefix, up });
            }
        }
        if !out.is_empty() {
            return out;
        }
    }

    // fallback: enumerate every interface without masks
    if let Ok(ifas) = local_ip_address::list_afinet_netifas() {
        for (name, ip) in ifas {
            if let IpAddr::V4(v4) = ip {
                if usable_v4(v4) {
                    out.push(AdapterInfo {
                        name: format!("Адаптер {name}"),
                        ip: v4.to_string(),
                        prefix: 24,
                        up: true,
                    });
                }
            }
        }
    }
    if out.is_empty() {
        if let Ok(ip) = local_ip_address::local_ip() {
            if let IpAddr::V4(v4) = ip {
                out.push(AdapterInfo { name: "Адаптер".into(), ip: v4.to_string(), prefix: 24, up: true });
            }
        }
    }
    out
}

/// Stable signature of all usable adapter addresses — when it changes
/// (VPN connected, Wi-Fi switched) the background scan re-runs.
pub fn adapter_signature(adapters: &[AdapterInfo]) -> String {
    let mut keys: Vec<String> = adapters
        .iter()
        .map(|a| format!("{}|{}|{}", a.name, a.ip, a.up))
        .collect();
    keys.sort();
    keys.join(";")
}

/// All local IPv4 addresses (all adapters!); Radmin ones start with 26.
pub fn my_ips() -> Vec<String> {
    let mut out: Vec<String> = my_adapters().into_iter().map(|a| a.ip).collect();
    if out.is_empty() {
        if let Ok(ip) = local_ip_address::local_ip() {
            out.push(ip.to_string());
        }
    }
    let mut uniq: HashSet<String> = out.into_iter().collect();
    if let Ok(ifas) = local_ip_address::list_afinet_netifas() {
        for (_name, ip) in ifas {
            if let IpAddr::V4(v4) = ip {
                if !v4.is_loopback() {
                    uniq.insert(v4.to_string());
                }
            }
        }
    }
    let mut v: Vec<String> = uniq.into_iter().collect();
    v.sort();
    v
}

/// Host range (inclusive, network+1 .. broadcast-1) of the subnet at
/// `prefix` containing `ip`. For degenerate prefixes keeps the /24 window.
fn subnet_range(ip: Ipv4Addr, prefix: u8) -> (u32, u32) {
    let p = prefix.clamp(0, 24);
    let m = prefix_mask(p);
    let net = to_u32(ip) & m;
    let bcast = net | !m;
    let first = if p >= 31 { net } else { net + 1 };
    let last = if p >= 31 { bcast } else { bcast.saturating_sub(1) };
    (first, last)
}

/// /24 window around a given address (a.b.c.1-254)
fn window24(ip: Ipv4Addr) -> Vec<String> {
    let (a, b) = subnet_range(ip, 24);
    (a..=b).map(|x| from_u32(x).to_string()).collect()
}

/// Compute the full list of addresses to sweep.
/// Rules: subnet <= /22 (up to 1024 hosts) is scanned entirely; larger masks
/// (Radmin /8, corporate /16, CGNAT 100.64/10) get a /24 window around our own
/// address plus /24 windows around known contacts and manually added subnets.
pub fn scan_targets(adapters: &[AdapterInfo], known: &[String], manual: &[String]) -> Vec<String> {
    let mut set: HashSet<String> = HashSet::new();

    for a in adapters {
        if !a.up {
            continue;
        }
        let Ok(v4) = a.ip.parse::<Ipv4Addr>() else { continue };
        if !usable_v4(v4) {
            continue;
        }
        let p = a.prefix.clamp(0, 32);
        if p <= 22 {
            // whole subnet, capped at 1024 hosts
            let (first, last) = subnet_range(v4, p);
            if last > first {
                let n = (last - first + 1) as u64;
                if n <= 1024 {
                    for x in first..=last {
                        set.insert(from_u32(x).to_string());
                    }
                    continue;
                }
            }
        }
        for t in window24(v4) {
            set.insert(t);
        }
    }

    // /24 around every known contact address
    for k in known {
        let Ok(v4) = k.parse::<Ipv4Addr>() else { continue };
        if !usable_v4(v4) {
            continue;
        }
        for t in window24(v4) {
            set.insert(t);
        }
    }

    // manual entries: plain IP or CIDR (a.b.c.d/p)
    for m in manual {
        let entry = m.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((addr, plen)) = entry.split_once('/') {
            let (Ok(v4), Ok(p)) = (addr.trim().parse::<Ipv4Addr>(), plen.trim().parse::<u8>()) else {
                continue;
            };
            let p = p.clamp(0, 32);
            if p >= 24 {
                for t in window24(v4) {
                    set.insert(t);
                }
            } else if p >= 8 {
                let (first, last) = subnet_range(v4, p);
                if last > first {
                    let n = (last - first + 1) as u64;
                    if n <= 1024 {
                        for x in first..=last {
                            set.insert(from_u32(x).to_string());
                        }
                        continue;
                    }
                }
                for t in window24(v4) {
                    set.insert(t);
                }
            } else {
                set.insert(v4.to_string());
            }
        } else if entry.parse::<Ipv4Addr>().map(usable_v4).unwrap_or(false) {
            set.insert(entry.to_string());
        }
    }

    // hard cap against pathological configurations
    let mut v: Vec<String> = set.into_iter().take(8192).collect();
    v.sort();
    v
}

/// Human-readable scan coverage for network diagnostics.
pub fn scan_nets() -> Vec<String> {
    let ads = my_adapters();
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for a in ads.iter().filter(|a| a.up) {
        let Ok(v4) = a.ip.parse::<Ipv4Addr>() else { continue };
        if !usable_v4(v4) {
            continue;
        }
        let desc = if a.prefix <= 22 {
            let (first, last) = subnet_range(v4, a.prefix.clamp(0, 32));
            let n = if last > first { (last - first + 1) as u64 } else { 0 };
            if n > 0 && n <= 1024 {
                format!("{}: {}/{} — вся подсеть ({} адресов)", a.name, a.ip, a.prefix, n)
            } else {
                format!("{}: {} — окно /24", a.name, a.ip)
            }
        } else {
            format!("{}: {} — окно /24", a.name, a.ip)
        };
        if seen.insert(desc.clone()) {
            out.push(desc);
        }
    }
    out
}

/// TCP ping: time to connect to peer port. None = offline.
pub async fn tcp_ping(ip: &str, port: u16, timeout_ms: u64) -> Option<u64> {
    let addr = format!("{ip}:{port}");
    let t0 = Instant::now();
    let res = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        TcpStream::connect(&addr),
    ).await;
    match res {
        Ok(Ok(_)) => Some(t0.elapsed().as_millis() as u64),
        _ => None,
    }
}

/// Sweep `targets` concurrently (128 workers, 450 ms probe timeout),
/// reporting progress via `safedrop-scan-progress` {done, total}.
/// Returns Vec<(ip, ping_ms)> of live hosts.
pub async fn scan_subnet_with_progress(
    port: u16,
    targets: Vec<String>,
    app: Option<tauri::AppHandle>,
) -> Vec<(String, u64)> {
    let total = targets.len();
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(128));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = std::sync::Arc::new(app);
    let mut tasks = Vec::with_capacity(total);
    for ip in targets {
        let sem = sem.clone();
        let done = done.clone();
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            let _p = sem.acquire().await.ok();
            let ping = tcp_ping(&ip, port, 450).await;
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if n % 64 == 0 || n == total {
                if let Some(a) = app.as_ref() {
                    let _ = a.emit("safedrop-scan-progress", serde_json::json!({ "done": n, "total": total }));
                }
            }
            ping.map(|ms| (ip, ms))
        }));
    }
    let mut live = Vec::new();
    for t in tasks {
        if let Ok(Some((ip, ping))) = t.await {
            live.push((ip, ping));
        }
    }
    if let Some(a) = app.as_ref() {
        let _ = a.emit("safedrop-scan-progress", serde_json::json!({ "done": total, "total": total }));
    }
    live.sort();
    live
}
