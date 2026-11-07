"""Finding Ollama servers: address handling, scan targets, probing a fake server."""

import asyncio
import ipaddress
import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

from fane.discovery import (NotOllama, OllamaServer, normalize_host, port_open, probe,
                            same_server, scan_targets)
from fane.llm import choose_model


@pytest.mark.parametrize("text, expected", [
    ("192.168.1.20", "http://192.168.1.20:11434"),
    ("192.168.1.20:8080", "http://192.168.1.20:8080"),
    ("http://gpu-box.lan:11434/", "http://gpu-box.lan:11434"),
    ("https://ollama.example.com", "https://ollama.example.com"),
    ("  desktop.local  ", "http://desktop.local:11434"),
])
def test_normalize_host(text, expected):
    assert normalize_host(text) == expected


@pytest.mark.parametrize("text", ["", "   ", "ftp://host", "http://"])
def test_normalize_host_rejects(text):
    with pytest.raises(NotOllama):
        normalize_host(text)


def test_same_server():
    assert same_server("http://localhost:11434", "127.0.0.1")
    assert same_server("http://127.0.0.1:11434/", "http://127.0.0.1:11434")
    assert not same_server("127.0.0.1", "192.168.1.5")
    assert not same_server("192.168.1.5:11434", "192.168.1.5:8080")


def test_server_matches_aliases():
    s = OllamaServer(url="http://127.0.0.1:11434", local=True,
                     aliases=["http://192.168.1.173:11434"])
    assert s.matches("http://192.168.1.173:11434")
    assert s.matches("localhost")
    assert not s.matches("192.168.1.9")
    assert s.label == "This computer"


def test_scan_targets():
    nets = [("lo", ipaddress.IPv4Interface("127.0.0.1/8")),
            ("wlo1", ipaddress.IPv4Interface("192.168.1.173/24")),
            ("docker0", ipaddress.IPv4Interface("172.17.0.1/16")),
            ("eth0", ipaddress.IPv4Interface("10.20.30.40/16")),
            ("tailscale0", ipaddress.IPv4Interface("100.101.102.103/32"))]
    targets, own = scan_targets(nets)
    assert own == {"127.0.0.1", "192.168.1.173", "172.17.0.1", "10.20.30.40", "100.101.102.103"}
    assert "192.168.1.1" in targets and "192.168.1.254" in targets
    assert "192.168.1.173" not in targets  # ourselves
    assert not any(t.startswith("172.17.") for t in targets)  # container bridge
    assert "10.20.30.1" in targets and "10.20.31.1" not in targets  # /16 narrowed to our /24
    assert len(targets) == 253 + 253


def test_choose_model():
    cfg = {"name": "openbmb/minicpm-v4.7:35b-a3b", "fallbacks": ["gemma4:31b", "qwen3-vl"]}
    assert choose_model(cfg, ["minicpm-v4.7:35b-a3b", "gemma4:31b"]) == "minicpm-v4.7:35b-a3b"
    assert choose_model(cfg, ["llama3:8b", "qwen3-vl:latest"]) == "qwen3-vl:latest"
    assert choose_model(cfg, ["llama3:8b"]) is None


class _FakeOllama(BaseHTTPRequestHandler):
    def do_GET(self):
        bodies = {"/api/version": {"version": "0.99.0"},
                  "/api/tags": {"models": [{"name": "gemma4:31b"}, {"name": "llama3:8b"}]}}
        body = bodies.get(self.path)
        self.send_response(200 if body else 404)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(body or {}).encode())

    def log_message(self, *a):
        pass


class _NotOllama(_FakeOllama):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"<html>router login</html>")


def _serve(handler):
    srv = HTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, f"http://127.0.0.1:{srv.server_port}"


def test_probe_fake_ollama():
    srv, url = _serve(_FakeOllama)
    try:
        server = asyncio.run(probe(url))
        assert server.version == "0.99.0"
        assert server.models == ["gemma4:31b", "llama3:8b"]
        assert asyncio.run(port_open("127.0.0.1", srv.server_port, 1.0))
    finally:
        srv.shutdown()


def test_probe_rejects_other_servers():
    srv, url = _serve(_NotOllama)
    try:
        with pytest.raises(NotOllama, match="isn't Ollama"):
            asyncio.run(probe(url))
    finally:
        srv.shutdown()
    with pytest.raises(NotOllama, match="no Ollama answering"):
        asyncio.run(probe(url))  # nothing listening any more
