"""Minimal async client for a local Ollama server."""

from __future__ import annotations

import base64
import io
import json
import logging
from typing import Any

import httpx
from PIL import Image

log = logging.getLogger(__name__)


def encode_image(img: Image.Image, max_side: int = 1280) -> str:
    """Downscale and JPEG-encode an image for the model; returns base64."""
    img = img.convert("RGB")
    scale = max_side / max(img.size)
    if scale < 1:
        img = img.resize((round(img.width * scale), round(img.height * scale)), Image.LANCZOS)
    buf = io.BytesIO()
    img.save(buf, format="JPEG", quality=82)
    return base64.b64encode(buf.getvalue()).decode()


class LLMError(RuntimeError):
    pass


class Ollama:
    def __init__(self, cfg: dict[str, Any]):
        self.cfg = cfg
        self.host = cfg["host"].rstrip("/")
        self.model = cfg["name"]
        self._client = httpx.AsyncClient(timeout=cfg["timeout"])

    async def installed_models(self) -> list[str]:
        r = await self._client.get(f"{self.host}/api/tags")
        r.raise_for_status()
        return [m["name"] for m in r.json().get("models", [])]

    async def resolve_model(self) -> str:
        """Use the configured model if installed, else the first installed fallback."""
        try:
            installed = await self.installed_models()
        except httpx.HTTPError as e:
            raise LLMError(f"Ollama is not reachable at {self.host}: {e}") from e

        def find(name: str) -> str | None:
            base = name.split(":")[0]
            for m in installed:
                if m == name or m.split(":")[0] == base:
                    return m
            for m in installed:
                if m.startswith(base):
                    return m
            return None

        for candidate in [self.cfg["name"], *self.cfg["fallbacks"]]:
            if found := find(candidate):
                if found.split(":")[0] != self.cfg["name"].split(":")[0]:
                    log.warning("model %r not installed; using %r", self.cfg["name"], found)
                self.model = found
                return found
        raise LLMError(
            f"No vision model installed. Try: ollama pull {self.cfg['name']}"
            f" (installed: {', '.join(installed) or 'none'})"
        )

    async def chat(
        self,
        messages: list[dict[str, Any]],
        *,
        tools: list[dict[str, Any]] | None = None,
        schema: dict[str, Any] | None = None,
        temperature: float | None = None,
    ) -> dict[str, Any]:
        """Single non-streaming chat turn. Returns the assistant message dict."""
        body: dict[str, Any] = {
            "model": self.model,
            "messages": messages,
            "stream": False,
            "think": False,
            "keep_alive": self.cfg["keep_alive"],
            "options": {"temperature": self.cfg["temperature"] if temperature is None else temperature,
                        "num_ctx": self.cfg["num_ctx"]},
        }
        if tools:
            body["tools"] = tools
        if schema:
            body["format"] = schema
        try:
            r = await self._client.post(f"{self.host}/api/chat", json=body)
        except httpx.HTTPError as e:
            raise LLMError(f"Ollama request failed: {e}") from e
        if r.status_code == 400 and "think" in r.text:
            # Model doesn't know the `think` switch; retry without it.
            body.pop("think")
            r = await self._client.post(f"{self.host}/api/chat", json=body)
        if r.status_code >= 400:
            raise LLMError(f"Ollama error {r.status_code}: {r.text[:300]}")
        return r.json()["message"]

    async def chat_json(self, messages: list[dict[str, Any]], schema: dict[str, Any]) -> Any:
        msg = await self.chat(messages, schema=schema)
        text = msg.get("content", "").strip()
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            start, end = text.find("{"), text.rfind("}")
            if start >= 0 and end > start:
                return json.loads(text[start : end + 1])
            raise LLMError(f"model did not return JSON: {text[:200]}")

    async def aclose(self) -> None:
        await self._client.aclose()
