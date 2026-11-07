"""Connect to configured stdio MCP servers and expose their tools to the model."""

from __future__ import annotations

import asyncio
import base64
import logging
import os
import re
import tempfile
from typing import Any

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from fane.connectors import Tool, ToolRegistry

log = logging.getLogger(__name__)


def _safe(name: str) -> str:
    return re.sub(r"[^a-zA-Z0-9_]", "_", name)


class MCPServer:
    """Keeps one server session open in a dedicated task (anyio scopes need that)."""

    def __init__(self, name: str, cfg: dict[str, Any]):
        self.name = name
        self.cfg = cfg
        self.session: ClientSession | None = None
        self._ready = asyncio.Event()
        self._stop = asyncio.Event()
        self._task: asyncio.Task | None = None
        self.error: str | None = None

    async def start(self, timeout: float = 30) -> bool:
        self._task = asyncio.create_task(self._run(), name=f"mcp:{self.name}")
        try:
            await asyncio.wait_for(self._ready.wait(), timeout)
        except asyncio.TimeoutError:
            self.error = "timed out starting"
        return self.session is not None

    async def _run(self) -> None:
        env = {**os.environ, **self.cfg.get("env", {})}
        params = StdioServerParameters(command=self.cfg["command"], args=self.cfg.get("args", []),
                                       env=env, cwd=self.cfg.get("cwd"))
        try:
            with open(os.devnull, "w") as errlog:
                async with stdio_client(params, errlog=errlog) as (read, write):
                    async with ClientSession(read, write) as session:
                        await session.initialize()
                        self.session = session
                        self._ready.set()
                        await self._stop.wait()
        except Exception as e:
            self.error = str(e)
            log.warning("MCP server %s failed: %s", self.name, e)
        finally:
            self.session = None
            self._ready.set()

    async def stop(self) -> None:
        self._stop.set()
        if self._task:
            try:
                await asyncio.wait_for(self._task, 5)
            except (asyncio.TimeoutError, Exception):
                self._task.cancel()


def _render_result(result: Any) -> str:
    parts = []
    for block in getattr(result, "content", []) or []:
        kind = getattr(block, "type", "")
        if kind == "text":
            parts.append(block.text)
        elif kind == "image":
            suffix = "." + (block.mime_type.split("/")[-1] if getattr(block, "mime_type", "") else "png")
            fd, path = tempfile.mkstemp(prefix="fane-mcp-", suffix=suffix)
            with os.fdopen(fd, "wb") as f:
                f.write(base64.b64decode(block.data))
            parts.append(f"[image saved to {path}; use insert_image to paste it]")
        else:
            parts.append(str(block))
    text = "\n".join(parts) or "(no content)"
    if getattr(result, "is_error", False):
        text = "error: " + text
    return text[:16000]


class MCPManager:
    def __init__(self, servers: dict[str, dict[str, Any]]):
        self.servers = {name: MCPServer(name, cfg) for name, cfg in servers.items() if cfg.get("command")}

    async def start(self, registry: ToolRegistry) -> None:
        await asyncio.gather(*(s.start() for s in self.servers.values()))
        for server in self.servers.values():
            if server.session is None:
                continue
            try:
                listed = await server.session.list_tools()
            except Exception as e:
                log.warning("MCP %s list_tools failed: %s", server.name, e)
                continue
            for t in listed.tools:
                registry.add(self._wrap(server, t))
            log.info("MCP %s: %d tools", server.name, len(listed.tools))

    def _wrap(self, server: MCPServer, t: Any) -> Tool:
        async def handler(**args: Any) -> str:
            if server.session is None:
                return f"error: MCP server {server.name} is not running"
            return _render_result(await server.session.call_tool(t.name, args))

        schema = t.input_schema or {"type": "object", "properties": {}}
        return Tool(name=f"{_safe(server.name)}__{_safe(t.name)}",
                    description=f"[{server.name}] {t.description or t.name}",
                    parameters=schema, handler=handler, status=f"Asking {server.name}")

    async def stop(self) -> None:
        await asyncio.gather(*(s.stop() for s in self.servers.values()))
