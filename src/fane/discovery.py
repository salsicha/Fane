"""Find Ollama servers on this computer and the local network.

Ollama doesn't advertise itself (no mDNS), so this probes the default port on every
address of the local subnets, then confirms each hit with Ollama's own API.
"""

from __future__ import annotations

import asyncio
import ipaddress
import json
import logging
import subprocess
from dataclasses import dataclass, field

import httpx

log = logging.getLogger(__name__)

DEFAULT_PORT = 11434
# Container/VM bridges: anything on them is reachable through localhost anyway.
SKIP_IFACES = ("lo", "docker", "br-", "veth", "virbr", "podman", "lxc")


class NotOllama(ValueError):
    pass


@dataclass
class OllamaServer:
    url: str
    name: str = ""  # reverse-DNS name, if any
    version: str = ""
    models: list[str] = field(default_factory=list)
    local: bool = False  # this computer
    aliases: list[str] = field(default_factory=list)  # other URLs that reach it

    def matches(self, host: str) -> bool:
        """True if the `host` setting points at this server."""
        return any(same_server(u, host) for u in [self.url, *self.aliases])

    @property
    def address(self) -> str:
        return httpx.URL(self.url).host

    @property
    def label(self) -> str:
        if self.local:
            return "This computer"
        return self.name or self.address


def normalize_host(text: str, port: int = DEFAULT_PORT) -> str:
    """'192.168.1.5' -> 'http://192.168.1.5:11434'; keeps an explicit scheme and port."""
    text = text.strip()
    if not text:
        raise NotOllama("enter an address, e.g. 192.168.1.20")
    if "://" not in text:
        text = "http://" + text
    try:
        url = httpx.URL(text)
    except httpx.InvalidURL as e:
        raise NotOllama(f"not an address: {text}") from e
    if not url.host or url.scheme not in ("http", "https"):
        raise NotOllama(f"not an address: {text}")
    if url.port is None and url.scheme == "http":
        url = url.copy_with(port=port)
    return str(url.copy_with(path="/", query=None, fragment=None)).rstrip("/")


def _canonical(url: str) -> str:
    try:
        u = httpx.URL(normalize_host(url))
    except NotOllama:
        return url
    if u.host in ("localhost", "::1") or u.host.startswith("127."):
        u = u.copy_with(host="127.0.0.1")
    return str(u)


def same_server(a: str, b: str) -> bool:
    """True if two host settings point at the same server ('localhost' == '127.0.0.1')."""
    return _canonical(a) == _canonical(b)


def local_networks() -> list[tuple[str, ipaddress.IPv4Interface]]:
    """(interface name, address/prefix) for every IPv4 address on an interface that's up."""
    try:
        out = subprocess.run(["ip", "-j", "-4", "addr", "show", "up"],
                             capture_output=True, text=True, timeout=3).stdout
        links = json.loads(out or "[]")
    except (OSError, subprocess.SubprocessError, ValueError) as e:
        log.warning("cannot list network interfaces: %s", e)
        return []
    return [(link.get("ifname", ""), ipaddress.IPv4Interface(f"{a['local']}/{a['prefixlen']}"))
            for link in links for a in link.get("addr_info", []) if a.get("family") == "inet"]


def scan_targets(nets: list[tuple[str, ipaddress.IPv4Interface]]) -> tuple[list[str], set[str]]:
    """Addresses worth probing, and this computer's own addresses.

    Networks bigger than a /24 are narrowed to the /24 around our own address, which keeps
    a scan to about a second.
    """
    own = {str(iface.ip) for _, iface in nets}
    targets: list[str] = []
    for name, iface in nets:
        if iface.ip.is_loopback or name.startswith(SKIP_IFACES):
            continue
        net = iface.network
        if net.prefixlen < 24:
            net = ipaddress.IPv4Interface(f"{iface.ip}/24").network
        targets += [str(h) for h in net.hosts() if str(h) not in own]
    return list(dict.fromkeys(targets)), own


async def port_open(host: str, port: int, timeout: float) -> bool:
    try:
        _, writer = await asyncio.wait_for(asyncio.open_connection(host, port), timeout)
    except (OSError, asyncio.TimeoutError):
        return False
    writer.close()
    try:
        await writer.wait_closed()
    except OSError:
        pass
    return True


async def probe(url: str, client: httpx.AsyncClient | None = None,
                timeout: float = 3.0) -> OllamaServer:
    """Ask `url` for its Ollama version and models. Raises NotOllama if it isn't one."""
    own_client = client is None
    client = client or httpx.AsyncClient(timeout=timeout)
    try:
        v = await client.get(f"{url}/api/version")
        v.raise_for_status()
        version = v.json()["version"]
        t = await client.get(f"{url}/api/tags")
        t.raise_for_status()
        models = [m["name"] for m in t.json().get("models", [])]
    except httpx.HTTPError as e:
        raise NotOllama(f"no Ollama answering at {url} ({type(e).__name__})") from e
    except (ValueError, KeyError, TypeError) as e:
        raise NotOllama(f"{url} answered, but it isn't Ollama") from e
    finally:
        if own_client:
            await client.aclose()
    return OllamaServer(url=url, version=version, models=models)


async def _hostname(address: str) -> str:
    try:
        name, _ = await asyncio.wait_for(
            asyncio.get_running_loop().getnameinfo((address, 0)), 1.0)
    except (OSError, asyncio.TimeoutError):
        return ""
    return "" if name == address else name


async def discover(extra: list[str] = (), port: int = DEFAULT_PORT,
                   timeout: float = 0.5, concurrency: int = 256) -> list[OllamaServer]:
    """Every Ollama server on this computer or the local subnets, this computer first.

    `extra` URLs (e.g. the configured host, which may be on another network) are always
    probed too.
    """
    nets = await asyncio.to_thread(local_networks)
    targets, own = scan_targets(nets)
    sem = asyncio.Semaphore(concurrency)

    async def check(host: str) -> str | None:
        async with sem:
            return host if await port_open(host, port, timeout) else None

    hits = [h for h in await asyncio.gather(*(check(h) for h in targets)) if h]
    log.info("scanned %d addresses for Ollama on port %d: %d answered", len(targets), port, len(hits))

    urls = [f"http://127.0.0.1:{port}", *(f"http://{h}:{port}" for h in hits)]
    aliases: dict[str, list[str]] = {}
    for given in extra:
        try:
            url = normalize_host(given, port)
        except NotOllama:
            continue
        if httpx.URL(url).host in own:  # our own LAN address: same server as localhost
            url = str(httpx.URL(url).copy_with(host="127.0.0.1"))
        url = _canonical(url)
        aliases.setdefault(url, []).append(given)
        urls.append(url)
    urls = list(dict.fromkeys(urls))

    async with httpx.AsyncClient(timeout=3.0) as client:
        async def one(url: str) -> OllamaServer | None:
            try:
                server = await probe(url, client)
            except NotOllama as e:
                log.debug("%s", e)
                return None
            server.local = server.address.startswith("127.")
            server.aliases = aliases.get(url, [])
            if not server.local:
                server.name = await _hostname(server.address)
            return server

        found = [s for s in await asyncio.gather(*(one(u) for u in urls)) if s]
    return sorted(found, key=lambda s: (not s.local, s.label.lower()))
