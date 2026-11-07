"""Hand hard tasks to Claude Code or Codex running non-interactively."""

from __future__ import annotations

import asyncio
import shutil
from typing import Any

from fane.config import expand


class Delegate:
    def __init__(self, cfg: dict[str, Any]):
        self.cfg = cfg

    def available(self) -> list[str]:
        return [name for name in ("claude", "codex") if shutil.which(self.cfg[f"{name}_cmd"][0])]

    async def run(self, agent: str, task: str, cwd: str = "") -> str:
        agent = agent.lower().strip()
        if agent not in ("claude", "codex"):
            return "error: agent must be 'claude' or 'codex'"
        cmd = list(self.cfg[f"{agent}_cmd"])
        if not shutil.which(cmd[0]):
            return f"error: {cmd[0]} is not installed"
        workdir = expand(cwd or self.cfg["cwd"])
        proc = await asyncio.create_subprocess_exec(
            *cmd, task, cwd=workdir, stdin=asyncio.subprocess.DEVNULL,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT)
        try:
            out, _ = await asyncio.wait_for(proc.communicate(), self.cfg["timeout"])
        except asyncio.TimeoutError:
            proc.kill()
            return f"error: {agent} timed out after {self.cfg['timeout']}s"
        text = out.decode(errors="replace").strip()
        if len(text) > 12000:
            text = text[:2000] + "\n...\n" + text[-10000:]
        return text or f"{agent} finished with no output (exit {proc.returncode})"
