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


class ToolsUnsupported(LLMError):
    """The model has no native tool calling (e.g. MiniCPM-V); use the JSON protocol instead."""


def _parts(name: str) -> tuple[str, str]:
    """'openbmb/minicpm-v4.7:35b-a3b' -> ('minicpm-v4.7', '35b-a3b'); tag '' if absent."""
    repo, _, tag = name.partition(":")
    return repo.rsplit("/", 1)[-1], tag


class Ollama:
    def __init__(self, cfg: dict[str, Any]):
        self.cfg = cfg
        self.host = cfg["host"].rstrip("/")
        self.model = cfg["name"]
        self._client = httpx.AsyncClient(timeout=cfg["timeout"])
        self._no_think: set[str] = set()  # models that reject the `think` switch
        # Timing/token counts from the most recent call, and running totals (used by `fane eval`).
        self.last_stats: dict[str, Any] = {}
        self.totals = {"calls": 0, "eval_count": 0, "thinking_chars": 0}

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

        for candidate in [self.cfg["name"], *self.cfg["fallbacks"]]:
            if found := find_installed(candidate, installed):
                if candidate != self.cfg["name"]:
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
            "keep_alive": self.cfg["keep_alive"],
            "options": {"temperature": self.cfg["temperature"] if temperature is None else temperature,
                        "num_ctx": self.cfg["num_ctx"]},
        }
        if self.model not in self._no_think:
            body["think"] = False  # suggestions and tool calls want speed, not long reasoning
        if tools:
            body["tools"] = tools
        if schema:
            body["format"] = schema
            # Format-constrained decoding occasionally degenerates into endless whitespace.
            body["options"]["num_predict"] = self.cfg["json_max_tokens"]
        r = await self._post(body)
        if r.status_code == 400 and "think" in r.text and "think" in body:
            # Model doesn't know the `think` switch; remember and retry without it.
            self._no_think.add(self.model)
            body.pop("think")
            r = await self._post(body)
        if r.status_code >= 400:
            if tools and "does not support tools" in r.text:
                raise ToolsUnsupported(f"{self.model} does not support native tool calling")
            raise LLMError(f"Ollama error {r.status_code}: {r.text[:300]}")
        data = r.json()
        self.last_stats = {k: data.get(k, 0) for k in (
            "total_duration", "load_duration", "prompt_eval_count", "prompt_eval_duration",
            "eval_count", "eval_duration")}
        msg = data["message"]
        self.totals["calls"] += 1
        self.totals["eval_count"] += data.get("eval_count", 0)
        # Some models reason even when asked not to; that's where latency hides.
        self.totals["thinking_chars"] += len(msg.get("thinking") or "")
        return msg

    async def _post(self, body: dict[str, Any]) -> httpx.Response:
        try:
            return await self._client.post(f"{self.host}/api/chat", json=body)
        except httpx.HTTPError as e:
            raise LLMError(f"Ollama request failed: {type(e).__name__} {e}") from e

    async def unload(self) -> None:
        """Ask Ollama to free this model's memory now."""
        try:
            await self._client.post(f"{self.host}/api/generate",
                                    json={"model": self.model, "keep_alive": 0})
        except httpx.HTTPError:
            pass

    async def loaded(self) -> dict[str, Any] | None:
        """This model's entry in /api/ps (size and how much of it is in VRAM)."""
        r = await self._client.get(f"{self.host}/api/ps")
        for m in r.json().get("models", []):
            if m.get("name") == self.model or m.get("model") == self.model:
                return m
        return None

    async def chat_json(self, messages: list[dict[str, Any]], schema: dict[str, Any]) -> Any:
        msg = await self.chat(messages, schema=schema)
        return parse_json(msg.get("content", ""))

    async def aclose(self) -> None:
        await self._client.aclose()


def find_installed(name: str, installed: list[str]) -> str | None:
    """Match a wanted model against `ollama list`, ignoring namespaces and ':latest'."""
    repo, tag = _parts(name)
    exact = [m for m in installed if _parts(m) == (repo, tag or "latest") or m == name]
    if exact:
        return exact[0]
    if not tag:  # any tag of that repo
        same = [m for m in installed if _parts(m)[0] == repo]
        if same:
            return same[0]
    return None


def parse_json(text: str) -> Any:
    text = text.strip()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        start, end = text.find("{"), text.rfind("}")
        if start >= 0 and end > start:
            try:
                return json.loads(text[start : end + 1])
            except json.JSONDecodeError:
                pass
        raise LLMError(f"model did not return JSON: {text[:200]}")
