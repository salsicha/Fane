//! Find model servers on this computer and the local network: other computers running Fane
//! (which share their models), Ollama, llama.cpp's server and stable-diffusion.cpp's server.
//!
//! None of them advertise themselves (no mDNS), so this probes their default ports on every
//! address of the local subnets, then confirms each hit with the server's own API.

use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use futures::stream::{self, StreamExt};
use serde::Serialize;
use serde_json::Value;

pub const DEFAULT_PORT: u16 = 11434;
/// Fane's model server, Ollama, stable-diffusion.cpp's server, llama.cpp's server.
pub const SCAN_PORTS: [u16; 4] = [11500, 11434, 1234, 8080];
/// Container/VM bridges: anything on them is reachable through localhost anyway.
const SKIP_IFACES: [&str; 7] = ["lo", "docker", "br-", "veth", "virbr", "podman", "lxc"];

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct NotServer(pub String);

#[derive(Debug, Clone, Default, Serialize)]
pub struct Server {
    pub url: String,
    /// "fane", "ollama", "openai" (llama.cpp and the like) or "sd".
    pub kind: String,
    /// A Fane that wants its sharing token before it says more.
    pub locked: bool,
    /// Reverse-DNS name, if any.
    pub name: String,
    pub version: String,
    pub models: Vec<String>,
    /// This computer.
    pub local: bool,
    /// Other URLs that reach the same server (e.g. our own LAN address).
    pub aliases: Vec<String>,
}

impl Server {
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

/// A server's kind and models, as Fane routes to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// "fane", "openai" (Ollama and other OpenAI-compatible servers) or "sd".
    pub kind: String,
    pub models: Vec<String>,
}

/// What a configured remote is (`kind` "" = find out) and which models it has. Fane remotes
/// are asked with their sharing token.
pub async fn identify(url: &str, token: &str, kind: &str) -> Result<Found, String> {
    let c = client(Duration::from_secs(5));
    let url = url.trim_end_matches('/');
    let get = |path: &str| {
        let mut r = c.get(format!("{url}{path}"));
        if !token.is_empty() {
            r = r.bearer_auth(token);
        }
        async move {
            let r = r.send().await.map_err(|e| format!("{url} isn't answering ({})", self::kind(&e)))?;
            if r.status() == reqwest::StatusCode::UNAUTHORIZED {
                return Err(format!("{url} wants a different sharing token"));
            }
            let r = r.error_for_status().map_err(|e| format!("{url}: {e}"))?;
            r.json::<Value>().await.map_err(|_| format!("{url} answered, but not like a model server"))
        }
    };
    let models_of = |v: &Value| -> Vec<String> {
        v["data"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    let kind = if kind.is_empty() {
        if get("/fane/v1/info").await.is_ok_and(|v| v.get("fane").is_some()) {
            "fane"
        } else if get("/sdcpp/v1/models/status").await.is_ok_and(|v| v.get("state").is_some()) {
            "sd"
        } else {
            "openai"
        }
    } else if kind == "ollama" {
        "openai"
    } else {
        kind
    };
    let models = match kind {
        "sd" => {
            get("/sdcpp/v1/models/status").await?;
            vec![]
        }
        _ => models_of(&get("/v1/models").await?),
    };
    Ok(Found { kind: kind.to_string(), models })
}

/// '192.168.1.5' -> 'http://192.168.1.5:11434'; keeps an explicit scheme and port.
pub fn normalize_host(text: &str) -> Result<String, NotServer> {
    normalize_with_port(text, DEFAULT_PORT)
}

pub fn normalize_with_port(text: &str, port: u16) -> Result<String, NotServer> {
    let text = text.trim();
    if text.is_empty() {
        return Err(NotServer("enter an address, e.g. 192.168.1.20".into()));
    }
    let with_scheme = if text.contains("://") { text.to_string() } else { format!("http://{text}") };
    let bad = || NotServer(format!("not an address: {text}"));
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
    crate::llm::http_client(timeout)
}

/// Ask `url` for its Ollama version and models. Errors if it isn't an Ollama server.
pub async fn probe(url: &str) -> Result<Server, NotServer> {
    probe_with(&client(Duration::from_secs(3)), url).await
}

async fn probe_with(c: &reqwest::Client, url: &str) -> Result<Server, NotServer> {
    let get = |path: &str| {
        let full = format!("{url}{path}");
        async move {
            let r = c
                .get(&full)
                .send()
                .await
                .map_err(|e| NotServer(format!("nothing answering at {url} ({})", kind(&e))))?;
            let r =
                r.error_for_status().map_err(|_| NotServer(format!("{url} answered, but it isn't a model server")))?;
            r.json::<Value>().await.map_err(|_| NotServer(format!("{url} answered, but it isn't a model server")))
        }
    };
    let names = |v: &Value, list: &str, key: &str| -> Vec<String> {
        v[list]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m[key].as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    // Fane first (it also answers /v1/models), then Ollama, then the rest.
    if let Ok(info) = get("/fane/v1/info").await
        && let Some(version) = info["fane"].as_str()
    {
        let models = info["models"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m.as_str().map(String::from)).collect())
            .unwrap_or_default();
        return Ok(Server {
            url: url.to_string(),
            kind: "fane".into(),
            locked: info["locked"].as_bool().unwrap_or(false),
            name: info["name"].as_str().unwrap_or("").to_string(),
            version: version.to_string(),
            models,
            ..Default::default()
        });
    }
    match get("/api/version").await {
        Ok(v) if v["version"].is_string() => {
            let tags = get("/api/tags").await?;
            return Ok(Server {
                url: url.to_string(),
                kind: "ollama".into(),
                version: v["version"].as_str().unwrap_or("").to_string(),
                models: names(&tags, "models", "name"),
                ..Default::default()
            });
        }
        Err(e) if e.0.starts_with("nothing answering") => return Err(e),
        _ => {}
    }
    if let Ok(status) = get("/sdcpp/v1/models/status").await
        && status.get("state").is_some()
    {
        let recipe = status["recipe"].as_str().map(String::from).into_iter().collect();
        return Ok(Server { url: url.to_string(), kind: "sd".into(), models: recipe, ..Default::default() });
    }
    match get("/v1/models").await {
        Ok(v) if v["data"].is_array() => Ok(Server {
            url: url.to_string(),
            kind: "openai".into(),
            models: names(&v, "data", "id"),
            ..Default::default()
        }),
        _ => Err(NotServer(format!("{url} answered, but it isn't a model server"))),
    }
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
pub async fn discover(extra: &[String]) -> Vec<Server> {
    let found =
        futures::future::join_all(SCAN_PORTS.iter().map(|&port| discover_on(extra, port, Duration::from_millis(500))))
            .await;
    let mut all: Vec<Server> = vec![];
    for s in found.into_iter().flatten() {
        if !all.iter().any(|a| same_server(&a.url, &s.url)) {
            all.push(s);
        }
    }
    all.sort_by_key(|s| (!s.local, s.kind != "fane", s.label().to_lowercase()));
    all
}

pub async fn discover_on(extra: &[String], port: u16, timeout: Duration) -> Vec<Server> {
    let nets = tokio::task::spawn_blocking(local_networks).await.unwrap_or_default();
    let (targets, own) = scan_targets(&nets);
    let total = targets.len();
    let hits: Vec<Ipv4Addr> = stream::iter(targets)
        .map(|ip| async move { port_open(IpAddr::V4(ip), port, timeout).await.then_some(ip) })
        .buffer_unordered(256)
        .filter_map(|x| async move { x })
        .collect()
        .await;
    tracing::info!("scanned {total} addresses on port {port}: {} answered", hits.len());

    let mut urls = vec![format!("http://127.0.0.1:{port}")];
    urls.extend(hits.iter().map(|h| format!("http://{h}:{port}")));
    let mut aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for given in extra {
        let Ok(url) = normalize_with_port(given, port) else { continue };
        // Only addresses meant for this port (or without one).
        if !given.is_empty() && url::Url::parse(&url).ok().and_then(|u| u.port()) != Some(port) {
            continue;
        }
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
            if server.local && server.kind == "fane" {
                return None; // this Fane itself
            }
            server.aliases = aliases.get(url).cloned().unwrap_or_default();
            if !server.local
                && server.name.is_empty()
                && let Ok(ip) = server.address().parse::<IpAddr>()
            {
                server.name = hostname(ip).await;
            }
            Some(server)
        }
    }))
    .await;
    let mut found: Vec<Server> = found.into_iter().flatten().collect();
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
        let s = Server {
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
                let body = if !ollama || req.starts_with("GET /fane") || req.starts_with("GET /sdcpp") {
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
        assert_eq!((s.version.as_str(), s.kind.as_str()), ("0.99.0", "ollama"));
        assert_eq!(s.models, vec!["gemma4:31b", "llama3:8b"]);
        task.abort();
    }

    #[tokio::test]
    async fn probe_rejects_other_servers() {
        let (url, task) = fake_server(false).await;
        assert!(probe(&url).await.unwrap_err().0.contains("isn't a model server"));
        task.abort();
        let _ = task.await;
        assert!(probe(&url).await.unwrap_err().0.contains("nothing answering"));
    }
}
