"""`fane eval`: compare local vision models on Fane's real prompts and tools."""

from __future__ import annotations

import asyncio
import datetime as dt
import json
import shutil
import statistics
import subprocess
import time
from pathlib import Path
from typing import Any, Callable

from fane.agent import Agent
from fane.config import Config
from fane.context.windows import WindowInfo
from fane.evals.scenarios import AGENT_TASKS, SUGGEST_CASES, Mocks
from fane.evals.scenes import SCENES
from fane.llm import LLMError, Ollama, find_installed
from fane.snapshot import Snapshot
from fane.suggester import Suggester


def _snap(scene: str) -> Snapshot:
    render, title, app = SCENES[scene]
    return Snapshot(window=WindowInfo(title=title, app=app), image=render())


def _tok_s(stats: dict[str, Any]) -> float | None:
    if stats.get("eval_count") and stats.get("eval_duration"):
        return stats["eval_count"] / (stats["eval_duration"] / 1e9)
    return None


def _median(xs: list[float]) -> float | None:
    return round(statistics.median(xs), 1) if xs else None


def _ratio(results: list[dict], key: str) -> tuple[int, int]:
    scored = [r for r in results if r.get(key) is not None]
    return sum(bool(r[key]) for r in scored), len(scored)


class EvalRunner:
    def __init__(self, cfg: Config, *, repeat: int = 1, only: str | None = None,
                 select: str | None = None, timeout: float = 600,
                 log: Callable[[str], None] = print):
        self.cfg = cfg
        self.repeat = max(1, repeat)
        self.only = only
        self.select = select
        self.timeout = timeout
        self.log = log
        self.min_conf = cfg["suggestions"]["min_confidence"]

    def _wanted(self, name: str) -> bool:
        return not self.select or self.select in name

    # --- one model ------------------------------------------------------------------
    async def evaluate(self, name: str) -> dict[str, Any]:
        report: dict[str, Any] = {"model": name, "status": "ok", "suggest": [], "agent": []}
        llm = Ollama({**self.cfg["model"], "name": name})
        try:
            installed = await llm.installed_models()
        except Exception as e:
            await llm.aclose()
            return {**report, "status": "error", "error": f"Ollama unreachable: {e}"}
        resolved = find_installed(name, installed)
        if not resolved:
            await llm.aclose()
            return {**report, "status": "missing", "error": f"not installed — run: ollama pull {name}"}
        llm.model = resolved
        report["resolved"] = resolved

        try:
            await self._unload_all(llm)
            self.log(f"\n▶ {resolved}: loading…")
            t0 = time.monotonic()
            await llm.chat([{"role": "user", "content": "Reply with OK."}])
            report["load_s"] = round(time.monotonic() - t0, 1)
            if ps := await llm.loaded():
                size, vram = ps.get("size", 0), ps.get("size_vram", 0)
                report["size_gb"] = round(size / 1e9, 1)
                report["vram_pct"] = round(100 * vram / size) if size else None
                report["context"] = ps.get("context_length")
            self.log(f"  loaded in {report['load_s']}s, {report.get('size_gb')} GB, "
                     f"{report.get('vram_pct')}% on GPU")

            if self.only in (None, "suggest"):
                for case in SUGGEST_CASES:
                    if self._wanted(case.scene):
                        for _ in range(self.repeat):
                            report["suggest"].append(await self._suggest_case(llm, case))
            if self.only in (None, "agent"):
                modes: dict[str, str] = {}
                for task in AGENT_TASKS:
                    if self._wanted(task.name):
                        for _ in range(self.repeat):
                            report["agent"].append(await self._agent_task(llm, task, modes))
                report["tool_mode"] = "/".join(sorted({a["mode"] for a in report["agent"] if "mode" in a}))
        except LLMError as e:
            report.update(status="error", error=str(e))
        finally:
            await llm.unload()
            await llm.aclose()
        report["summary"] = summarize(report)
        return report

    async def _unload_all(self, llm: Ollama) -> None:
        """Free VRAM (including the candidate itself) so every model gets a cold, equal start."""
        try:
            r = await llm._client.get(f"{llm.host}/api/ps")
            for m in r.json().get("models", []):
                await llm._client.post(f"{llm.host}/api/generate",
                                       json={"model": m["name"], "keep_alive": 0})
            for _ in range(20):  # unloading is asynchronous
                r = await llm._client.get(f"{llm.host}/api/ps")
                if not r.json().get("models"):
                    break
                await asyncio.sleep(0.5)
        except Exception:
            pass

    async def _suggest_case(self, llm: Ollama, case) -> dict[str, Any]:
        snap = _snap(case.scene)
        sug = Suggester(llm, self.cfg["suggestions"], self.cfg["model"]["max_image_side"])
        rec: dict[str, Any] = {"case": case.scene, "expect": case.expect}
        t0 = time.monotonic()
        try:
            activity, raw = await asyncio.wait_for(sug.propose(snap), self.timeout)
            rec["json_ok"] = True
        except asyncio.TimeoutError:
            activity, raw, rec["json_ok"], rec["error"] = "", [], False, "timeout"
        except LLMError as e:
            activity, raw, rec["json_ok"], rec["error"] = "", [], False, str(e)[:300]
        rec["latency_s"] = round(time.monotonic() - t0, 1)
        rec["tok_s"] = _tok_s(llm.last_stats) if rec["json_ok"] else None
        rec["activity"], rec["suggestions"] = activity, raw

        rec["understood"] = any(k in activity.lower() for k in case.activity_any)
        confident = []
        for s in raw:
            try:
                if float(s.get("confidence", 0) or 0) >= self.min_conf:
                    confident.append(s)
            except (TypeError, ValueError):
                pass
        if case.expect == "suggest":
            rec["behaved"] = any(k in (str(s.get("title", "")) + " " + str(s.get("prompt", ""))).lower()
                                 for s in confident for k in case.suggest_any)
        elif case.expect == "quiet":
            rec["behaved"] = not confident
        else:
            rec["behaved"] = None
        mark = lambda b: "–" if b is None else ("✓" if b else "✗")  # noqa: E731
        top = confident[0]["title"] if confident else "(quiet)"
        self.log(f"  suggest {case.scene:<12} understand {mark(rec['understood'])}  "
                 f"behave {mark(rec['behaved'])}  {rec['latency_s']:>6}s  {top}")
        return rec

    async def _agent_task(self, llm: Ollama, task, modes: dict[str, str]) -> dict[str, Any]:
        mocks = Mocks()
        agent = Agent(llm, mocks.registry(), self.cfg["model"])
        agent._modes = modes  # share what we learn about native tool support
        rec: dict[str, Any] = {"case": task.name, "prompt": task.prompt}
        before = dict(llm.totals)
        t0 = time.monotonic()
        try:
            result = await asyncio.wait_for(agent.run(task.prompt, _snap(task.scene), [], lambda s: None),
                                            self.timeout)
        except (asyncio.TimeoutError, LLMError) as e:
            rec.update(latency_s=round(time.monotonic() - t0, 1), error=str(e)[:300] or "timeout",
                       checks={k: False for k in task.checks}, passed=False)
            self.log(f"  agent   {task.name:<18} ERROR {rec['error'][:80]}")
            return rec
        rec["latency_s"] = round(time.monotonic() - t0, 1)
        rec.update({f"llm_{k}": llm.totals[k] - before[k] for k in before})
        rec["mode"] = result.mode
        rec["answer"] = result.text
        rec["calls"] = [{"name": c.name, "args": c.args} for c in result.calls]
        rec["inserted"] = mocks.inserted
        rec["delegated"] = mocks.delegated
        rec["checks"] = {k: bool(fn(result, mocks)) for k, fn in task.checks.items()}
        rec["passed"] = all(rec["checks"].values())
        ok = sum(rec["checks"].values())
        self.log(f"  agent   {task.name:<18} {ok}/{len(rec['checks'])} checks  {rec['latency_s']:>6}s  "
                 f"{rec['llm_eval_count']:>5} tok  [{', '.join(result.steps) or 'no tools'}]")
        return rec

    # --- all models -----------------------------------------------------------------
    async def run(self, models: list[str]) -> dict[str, Any]:
        started = dt.datetime.now()
        reports = [await self.evaluate(m) for m in models]
        return {
            "started": started.isoformat(timespec="seconds"),
            "duration_s": round((dt.datetime.now() - started).total_seconds()),
            "machine": machine_info(),
            "ollama_version": await ollama_version(self.cfg["model"]["host"]),
            "settings": {"repeat": self.repeat, "num_ctx": self.cfg["model"]["num_ctx"],
                         "temperature": self.cfg["model"]["temperature"],
                         "max_image_side": self.cfg["model"]["max_image_side"],
                         "min_confidence": self.min_conf,
                         "tool_mode": self.cfg["model"].get("tool_mode", "auto")},
            "models": reports,
            "recommendation": recommend(reports),
        }


def summarize(r: dict[str, Any]) -> dict[str, Any]:
    sug, ag = r["suggest"], r["agent"]
    und, beh_s, beh_q = _ratio(sug, "understood"), \
        _ratio([s for s in sug if s["expect"] == "suggest"], "behaved"), \
        _ratio([s for s in sug if s["expect"] == "quiet"], "behaved")
    checks = [v for a in ag for v in a.get("checks", {}).values()]
    frac = lambda p: p[0] / p[1] if p[1] else None  # noqa: E731
    parts = {"understanding": frac(und), "helpful": frac(beh_s), "quiet": frac(beh_q),
             "agent_checks": sum(checks) / len(checks) if checks else None}
    weights = {"understanding": 0.2, "helpful": 0.15, "quiet": 0.15, "agent_checks": 0.5}
    have = {k: v for k, v in parts.items() if v is not None}
    quality = (100 * sum(weights[k] * v for k, v in have.items()) / sum(weights[k] for k in have)
               if have else None)
    suggest_parts = [parts[k] for k in ("understanding", "helpful", "quiet") if parts[k] is not None]
    speeds = [s["tok_s"] for s in sug if s.get("tok_s")]
    return {
        "understanding": und, "helpful": beh_s, "quiet": beh_q,
        "json_ok": _ratio(sug, "json_ok"),
        "agent_tasks": (sum(bool(a.get("passed")) for a in ag), len(ag)),
        "agent_checks": (sum(checks), len(checks)),
        "suggest_score": round(sum(suggest_parts) / len(suggest_parts), 3) if suggest_parts else None,
        "quality": round(quality, 1) if quality is not None else None,
        "suggest_latency_s": _median([s["latency_s"] for s in sug if s.get("json_ok")]),
        "agent_latency_s": _median([a["latency_s"] for a in ag if "error" not in a]),
        "tok_s": round(statistics.mean(speeds), 1) if speeds else None,
        "tool_calls": _median([len(a["calls"]) for a in ag if "calls" in a]),
        "agent_tokens": _median([a["llm_eval_count"] for a in ag if "llm_eval_count" in a]),
        "thinking_chars": _median([a["llm_thinking_chars"] for a in ag if "llm_thinking_chars" in a]),
    }


def recommend(reports: list[dict[str, Any]]) -> dict[str, Any]:
    ok = [r for r in reports if r["status"] == "ok" and r["summary"]["quality"] is not None]
    if not ok:
        return {}
    best = max(ok, key=lambda r: r["summary"]["quality"])
    rec: dict[str, Any] = {"name": best["resolved"]}
    floor = (best["summary"]["suggest_score"] or 0) - 0.1
    fast = [r for r in ok if (r["summary"]["suggest_score"] or 0) >= floor
            and r["summary"]["suggest_latency_s"] is not None]
    if fast:
        quickest = min(fast, key=lambda r: r["summary"]["suggest_latency_s"])
        b, q = best["summary"]["suggest_latency_s"], quickest["summary"]["suggest_latency_s"]
        if quickest is not best and b and q and q < 0.7 * b:
            rec["suggest_name"] = quickest["resolved"]
    return rec


def machine_info() -> dict[str, Any]:
    info: dict[str, Any] = {}
    if shutil.which("nvidia-smi"):
        out = subprocess.run(["nvidia-smi", "--query-gpu=name,memory.total", "--format=csv,noheader"],
                             capture_output=True, text=True).stdout.strip()
        info["gpu"] = out
    try:
        kb = int(next(line for line in open("/proc/meminfo") if line.startswith("MemTotal")).split()[1])
        info["ram_gb"] = round(kb / 1e6)
    except (OSError, StopIteration, ValueError):
        pass
    return info


async def ollama_version(host: str) -> str:
    import httpx
    try:
        async with httpx.AsyncClient(timeout=5) as c:
            return (await c.get(f"{host.rstrip('/')}/api/version")).json().get("version", "?")
    except Exception:
        return "?"


# --- reporting -----------------------------------------------------------------------

def _frac(p) -> str:
    return f"{p[0]}/{p[1]}" if p and p[1] else "–"


def to_markdown(res: dict[str, Any]) -> str:
    models = res["models"]
    ok = [m for m in models if m["status"] == "ok"]
    s = res["settings"]
    lines = [f"# Fane model eval — {res['started']}", "",
             f"Machine: {res['machine'].get('gpu', 'unknown GPU')}, {res['machine'].get('ram_gb', '?')} GB RAM · "
             f"Ollama {res['ollama_version']} · num_ctx {s['num_ctx']} · temperature {s['temperature']} · "
             f"image ≤{s['max_image_side']}px · repeat ×{s['repeat']} · took {res['duration_s'] // 60} min", ""]
    for m in models:
        if m["status"] != "ok":
            lines.append(f"- **{m['model']}**: {m['status']} — {m.get('error', '')}")
    if not ok:
        return "\n".join(lines) + "\n"

    lines += ["", "## Summary", "",
              "| | " + " | ".join(m["resolved"] for m in ok) + " |",
              "|---|" + "---|" * len(ok)]
    rows = [
        ("**Quality score** (0–100)", lambda m: m["summary"]["quality"]),
        ("Screen understanding", lambda m: _frac(m["summary"]["understanding"])),
        ("Helpful suggestion when expected", lambda m: _frac(m["summary"]["helpful"])),
        ("Stays quiet when nothing to do", lambda m: _frac(m["summary"]["quiet"])),
        ("Valid suggestion JSON", lambda m: _frac(m["summary"]["json_ok"])),
        ("Agent tasks fully passed", lambda m: _frac(m["summary"]["agent_tasks"])),
        ("Agent checks passed", lambda m: _frac(m["summary"]["agent_checks"])),
        ("Tool calling", lambda m: m.get("tool_mode", "–")),
        ("Median suggestion latency (s)", lambda m: m["summary"]["suggest_latency_s"]),
        ("Median agent task latency (s)", lambda m: m["summary"]["agent_latency_s"]),
        ("Generation speed (tok/s)", lambda m: m["summary"]["tok_s"]),
        ("Median tool calls per agent task", lambda m: m["summary"]["tool_calls"]),
        ("Median tokens generated per agent task", lambda m: m["summary"]["agent_tokens"]),
        ("Median hidden reasoning per task (chars)", lambda m: m["summary"]["thinking_chars"]),
        ("Loaded size (GB) / on GPU", lambda m: f"{m.get('size_gb', '–')} / {m.get('vram_pct', '–')}%"),
        ("Cold load (s)", lambda m: m.get("load_s")),
    ]
    for label, fn in rows:
        lines.append(f"| {label} | " + " | ".join(str(fn(m) if fn(m) is not None else "–") for m in ok) + " |")

    rec = res.get("recommendation") or {}
    if rec:
        lines += ["", "## Recommended config", "", "```toml", "[model]", f'name = "{rec["name"]}"']
        if rec.get("suggest_name"):
            lines.append(f'suggest_name = "{rec["suggest_name"]}"   # faster for the proactive loop')
        lines += ["```", "",
                  "Quality = 20% understanding + 15% helpful + 15% quiet + 50% agent checks. "
                  "A separate suggestion model is proposed only if it is ≥30% faster with a suggestion "
                  "score within 0.1 of the best model's."]

    lines += ["", "## Per case", "", "| case | " + " | ".join(m["resolved"] for m in ok) + " |",
              "|---|" + "---|" * len(ok)]
    cases = [("suggest", c.scene) for c in SUGGEST_CASES] + [("agent", t.name) for t in AGENT_TASKS]
    for kind, name in cases:
        cells = []
        for m in ok:
            recs = [r for r in m[kind] if r["case"] == name]
            if not recs:
                cells.append("–")
                continue
            if kind == "suggest":
                cell = " ".join(("U" if r["understood"] else "u") + (
                    "" if r["behaved"] is None else ("✓" if r["behaved"] else "✗")) for r in recs)
            else:
                cell = " ".join(f"{sum(r['checks'].values())}/{len(r['checks'])}" for r in recs)
            lat = _median([r["latency_s"] for r in recs])
            cells.append(f"{cell} · {lat}s")
        lines.append(f"| {kind}: {name} | " + " | ".join(cells) + " |")
    lines += ["", "U/u = understood the screen or not; ✓/✗ = suggested (or stayed quiet) as expected."]

    lines += ["", "## Failures", ""]
    any_fail = False
    for m in ok:
        for r in m["suggest"]:
            if r["understood"] is False or r["behaved"] is False or not r["json_ok"]:
                any_fail = True
                sugs = "; ".join(f"{s.get('title')} ({s.get('confidence')})" for s in r["suggestions"]) or "none"
                lines.append(f"- **{m['resolved']}** suggest `{r['case']}`: activity “{r['activity'][:160]}” · "
                             f"suggestions: {sugs[:200]}" + (f" · error: {r['error']}" if r.get("error") else ""))
        for r in m["agent"]:
            if not r.get("passed"):
                any_fail = True
                failed = [k for k, v in r["checks"].items() if not v]
                calls = ", ".join(c["name"] for c in r.get("calls", [])) or "none"
                lines.append(f"- **{m['resolved']}** agent `{r['case']}`: failed {failed} · tools: {calls} · "
                             f"answer: “{(r.get('answer') or r.get('error', ''))[:160]}”")
    if not any_fail:
        lines.append("None.")
    return "\n".join(lines) + "\n"


def save(res: dict[str, Any], out_dir: Path) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "results.json").write_text(json.dumps(res, indent=2, ensure_ascii=False, default=str))
    md = out_dir / "report.md"
    md.write_text(to_markdown(res))
    scenes = out_dir / "scenes"
    scenes.mkdir(exist_ok=True)
    for name, (render, _, _) in SCENES.items():
        render().save(scenes / f"{name}.png")
    return md
