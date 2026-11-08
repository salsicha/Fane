//! Find Ollama servers on this computer and the local network.
//!
//! Ollama doesn't advertise itself (no mDNS), so this probes the default port on every
//! address of the local subnets, then confirms each hit with Ollama's own API.

use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use futures::stream::{self, StreamExt};
use serde::Serialize;
use serde_json::Value;

pub const DEFAULT_PORT: u16 = 11434;
/// Container/VM bridges: anything on them is reachable through localhost anyway.
const SKIP_IFACES: [&str; 7] = ["lo", "docker", "br-", "veth", "virbr", "podman", "lxc"];

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct NotOllama(pub String);

#[derive(Debug, Clone, Default, Serialize)]
pub struct OllamaServer {
    pub url: String,
    /// Reverse-DNS name, if any.
    pub name: String,
    pub version: String,
    pub models: Vec<String>,
    /// This computer.
    pub local: bool,
    /// Other URLs that reach the same server (e.g. our own LAN address).
    pub aliases: Vec<String>,
}

impl OllamaServer {
    /// True if the `host` setting points at this server.
    pub fn matches(&self, host: &str) -> bool {
        std::iter::once(&self.url).chain(&self.aliases).any(|u| same_server(u, host))
    }

    pub fn address(&self) -> String {
        url::Url::parse(&self.url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default()
    }

    pub fn label(&self) -> String {
        if self.local {
            "This computer".into()
        } else if !self.name.is_empty() {
            self.name.clone()
        } else {
            self.address()
        }
    }
}

/// '192.168.1.5' -> 'http://192.168.1.5:11434'; keeps an explicit scheme and port.
pub fn normalize_host(text: &str) -> Result<String, NotOllama> {
    normalize_with_port(text, DEFAULT_PORT)
}

fn normalize_with_port(text: &str, port: u16) -> Result<String, NotOllama> {
    let text = text.trim();
    if text.is_empty() {
        return Err(NotOllama("enter an address, e.g. 192.168.1.20".into()));
    }
    let with_scheme = if text.contains("://") { text.to_string() } else { format!("http://{text}") };
    let bad = || NotOllama(format!("not an address: {text}"));
    let url = url::Url::parse(&with_scheme).map_err(|_| bad())?;
    let host = url.host_str().filter(|h| !h.is_empty()).ok_or_else(bad)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(bad());
    }
    let port = match (url.port(), url.scheme()) {
        (Some(p), _) => format!(":{p}"),
        (None, "http") => format!(":{port}"),
        (None, _) => String::new(),
    };
    Ok(format!("{}://{host}{port}", url.scheme()))
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host == "[::1]" || host == "::1" || host.starts_with("127.")
}

fn canonical(u: &str) -> String {
    let Ok(norm) = normalize_host(u) else { return u.to_string() };
    let Ok(mut url) = url::Url::parse(&norm) else { return norm };
    if url.host_str().is_some_and(is_loopback_host) {
        let _ = url.set_host(Some("127.0.0.1"));
    }
    url.as_str().trim_end_matches('/').to_string()
}

/// True if two host settings point at the same server ('localhost' == '127.0.0.1').
pub fn same_server(a: &str, b: &str) -> bool {
    canonical(a) == canonical(b)
}

/// (interface name, address, prefix length) for every IPv4 address on an interface that's up.
pub fn local_networks() -> Vec<(String, Ipv4Addr, u8)> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else { return vec![] };
    ifaces
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some((i.name, v4.ip, v4.prefixlen)),
            _ => None,
        })
        .collect()
}

/// Addresses worth probing, and this computer's own addresses.
///
/// Networks bigger than a /24 are narrowed to the /24 around our own address, which keeps
/// a scan to about a second.
pub fn scan_targets(nets: &[(String, Ipv4Addr, u8)]) -> (Vec<Ipv4Addr>, HashSet<Ipv4Addr>) {
    let own: HashSet<Ipv4Addr> = nets.iter().map(|(_, ip, _)| *ip).collect();
    let mut targets = vec![];
    let mut seen = HashSet::new();
    for (name, ip, prefix) in nets {
        if ip.is_loopback() || ip.is_link_local() || SKIP_IFACES.iter().any(|s| name.starts_with(s)) {
            continue;
        }
        let prefix = (*prefix).max(24);
        if prefix >= 31 {
            continue; // point-to-point / single host (e.g. a VPN address): nothing to scan
        }
        let mask = u32::MAX << (32 - prefix);
        let net = u32::from(*ip) & mask;
        let broadcast = net | !mask;
        for n in (net + 1)..broadcast {
            let host = Ipv4Addr::from(n);
            if !own.contains(&host) && seen.insert(host) {
                targets.push(host);
            }
        }
    }
    (targets, own)
}

async fn port_open(host: IpAddr, port: u16, timeout: Duration) -> bool {
    matches!(tokio::time::timeout(timeout, tokio::net::TcpStream::connect((host, port))).await, Ok(Ok(_)))
}

fn client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder().timeout(timeout).build().expect("HTTP client")
}

/// Ask `url` for its Ollama version and models. Errors if it isn't an Ollama server.
pub async fn probe(url: &str) -> Result<OllamaServer, NotOllama> {
    probe_with(&client(Duration::from_secs(3)), url).await
}

async fn probe_with(c: &reqwest::Client, url: &str) -> Result<OllamaServer, NotOllama> {
    let get = |path: &str| {
        let full = format!("{url}{path}");
        async move {
            let r = c
                .get(&full)
                .send()
                .await
                .map_err(|e| NotOllama(format!("no Ollama answering at {url} ({})", kind(&e))))?;
            let r = r.error_for_status().map_err(|_| NotOllama(format!("{url} answered, but it isn't Ollama")))?;
            r.json::<Value>().await.map_err(|_| NotOllama(format!("{url} answered, but it isn't Ollama")))
        }
    };
    let version = get("/api/version").await?;
    let version = version["version"]
        .as_str()
        .ok_or_else(|| NotOllama(format!("{url} answered, but it isn't Ollama")))?
        .to_string();
    let tags = get("/api/tags").await?;
    let models = tags["models"]
        .as_array()
        .map(|a| a.iter().filter_map(|m| m["name"].as_str().map(String::from)).collect())
        .unwrap_or_default();
    Ok(OllamaServer { url: url.to_string(), version, models, ..Default::default() })
}

fn kind(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "connection refused or unreachable"
    } else {
        "request failed"
    }
}

async fn hostname(ip: IpAddr) -> String {
    let lookup = tokio::task::spawn_blocking(move || dns_lookup::lookup_addr(&ip).ok());
    match tokio::time::timeout(Duration::from_secs(1), lookup).await {
        Ok(Ok(Some(name))) if name != ip.to_string() => name,
        _ => String::new(),
    }
}

/// Every Ollama server on this computer or the local subnets, this computer first.
///
/// `extra` URLs (e.g. the configured host, which may be on another network) are always
/// probed too.
pub async fn discover(extra: &[String]) -> Vec<OllamaServer> {
    discover_on(extra, DEFAULT_PORT, Duration::from_millis(500)).await
}

pub async fn discover_on(extra: &[String], port: u16, timeout: Duration) -> Vec<OllamaServer> {
    let nets = tokio::task::spawn_blocking(local_networks).await.unwrap_or_default();
    let (targets, own) = scan_targets(&nets);
    let total = targets.len();
    let hits: Vec<Ipv4Addr> = stream::iter(targets)
        .map(|ip| async move { port_open(IpAddr::V4(ip), port, timeout).await.then_some(ip) })
        .buffer_unordered(256)
        .filter_map(|x| async move { x })
        .collect()
        .await;
    tracing::info!("scanned {total} addresses for Ollama on port {port}: {} answered", hits.len());

    let mut urls = vec![format!("http://127.0.0.1:{port}")];
    urls.extend(hits.iter().map(|h| format!("http://{h}:{port}")));
    let mut aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for given in extra {
        let Ok(url) = normalize_with_port(given, port) else { continue };
        let mut parsed = url::Url::parse(&url).expect("normalized URL parses");
        let own_addr = parsed.host_str().and_then(|h| h.parse::<Ipv4Addr>().ok()).is_some_and(|ip| own.contains(&ip));
        if own_addr {
            // Our own LAN address: the same server as localhost.
            let _ = parsed.set_host(Some("127.0.0.1"));
        }
        let url = canonical(parsed.as_str());
        aliases.entry(url.clone()).or_default().push(given.clone());
        urls.push(url);
    }
    let mut seen = HashSet::new();
    urls.retain(|u| seen.insert(u.clone()));

    let c = client(Duration::from_secs(3));
    let found = futures::future::join_all(urls.iter().map(|url| {
        let c = &c;
        let aliases = &aliases;
        async move {
            let mut server = match probe_with(c, url).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("{e}");
                    return None;
                }
            };
            server.local = server.address().starts_with("127.");
            server.aliases = aliases.get(url).cloned().unwrap_or_default();
            if !server.local
                && let Ok(ip) = server.address().parse::<IpAddr>()
            {
                server.name = hostname(ip).await;
            }
            Some(server)
        }
    }))
    .await;
    let mut found: Vec<OllamaServer> = found.into_iter().flatten().collect();
    found.sort_by_key(|s| (!s.local, s.label().to_lowercase()));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_host_cases() {
        let cases = [
            ("192.168.1.20", "http://192.168.1.20:11434"),
            ("192.168.1.20:8080", "http://192.168.1.20:8080"),
            ("http://gpu-box.lan:11434/", "http://gpu-box.lan:11434"),
            ("https://ollama.example.com", "https://ollama.example.com"),
            ("  desktop.local  ", "http://desktop.local:11434"),
        ];
        for (text, expected) in cases {
            assert_eq!(normalize_host(text).unwrap(), expected, "{text}");
        }
        for bad in ["", "   ", "ftp://host", "http://"] {
            assert!(normalize_host(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn same_server_cases() {
        assert!(same_server("http://localhost:11434", "127.0.0.1"));
        assert!(same_server("http://127.0.0.1:11434/", "http://127.0.0.1:11434"));
        assert!(!same_server("127.0.0.1", "192.168.1.5"));
        assert!(!same_server("192.168.1.5:11434", "192.168.1.5:8080"));
    }

    #[test]
    fn server_matches_aliases() {
        let s = OllamaServer {
            url: "http://127.0.0.1:11434".into(),
            local: true,
            aliases: vec!["http://192.168.1.173:11434".into()],
            ..Default::default()
        };
        assert!(s.matches("http://192.168.1.173:11434") && s.matches("localhost"));
        assert!(!s.matches("192.168.1.9"));
        assert_eq!(s.label(), "This computer");
    }

    #[test]
    fn scan_targets_cases() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        let nets = vec![
            ("lo".to_string(), ip("127.0.0.1"), 8),
            ("wlo1".to_string(), ip("192.168.1.173"), 24),
            ("docker0".to_string(), ip("172.17.0.1"), 16),
            ("eth0".to_string(), ip("10.20.30.40"), 16),
            ("tailscale0".to_string(), ip("100.101.102.103"), 32),
        ];
        let (targets, own) = scan_targets(&nets);
        assert_eq!(own.len(), 5);
        assert!(targets.contains(&ip("192.168.1.1")) && targets.contains(&ip("192.168.1.254")));
        assert!(!targets.contains(&ip("192.168.1.173")));
        assert!(!targets.iter().any(|t| t.octets()[..2] == [172, 17]));
        assert!(targets.contains(&ip("10.20.30.1")) && !targets.contains(&ip("10.20.31.1")));
        assert_eq!(targets.len(), 253 + 253);
    }

    /// A one-shot HTTP server answering like Ollama (or not).
    async fn fake_server(ollama: bool) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut buf = [0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let body = if !ollama {
                    "<html>router login</html>".to_string()
                } else if req.starts_with("GET /api/version") {
                    r#"{"version":"0.99.0"}"#.into()
                } else {
                    r#"{"models":[{"name":"gemma4:31b"},{"name":"llama3:8b"}]}"#.into()
                };
                let resp =
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (url, task)
    }

    #[tokio::test]
    async fn probe_fake_ollama() {
        let (url, task) = fake_server(true).await;
        let s = probe(&url).await.unwrap();
        assert_eq!(s.version, "0.99.0");
        assert_eq!(s.models, vec!["gemma4:31b", "llama3:8b"]);
        task.abort();
    }

    #[tokio::test]
    async fn probe_rejects_other_servers() {
        let (url, task) = fake_server(false).await;
        assert!(probe(&url).await.unwrap_err().0.contains("isn't Ollama"));
        task.abort();
        let _ = task.await;
        assert!(probe(&url).await.unwrap_err().0.contains("no Ollama answering"));
    }
}
