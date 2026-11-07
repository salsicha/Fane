"""Fast checks that need no model, GPU or desktop."""

import asyncio

import pytest
from PIL import Image

from fane.agent import AgentResult, ToolCall
from fane.context.screen import change_ratio
from fane.context.windows import WindowInfo
from fane.evals.scenarios import AGENT_TASKS, Mocks
from fane.evals.scenes import SCENES
from fane.llm import LLMError, find_installed, parse_json
from fane.snapshot import is_private

INSTALLED = ["qwen3-vl:latest", "openbmb/minicpm-v4.7:35b-a3b", "gemma4:31b"]


@pytest.mark.parametrize("wanted, expected", [
    ("openbmb/minicpm-v4.7:35b-a3b", "openbmb/minicpm-v4.7:35b-a3b"),
    ("minicpm-v4.7:35b-a3b", "openbmb/minicpm-v4.7:35b-a3b"),
    ("gemma4", "gemma4:31b"),
    ("gemma4:26b", None),
    ("qwen3-vl", "qwen3-vl:latest"),
    ("qwen3.8:27b", None),
])
def test_find_installed(wanted, expected):
    assert find_installed(wanted, INSTALLED) == expected


def test_parse_json_tolerates_wrapping():
    assert parse_json('Sure! {"a": 1} hope that helps') == {"a": 1}
    with pytest.raises(LLMError):
        parse_json("no json here")


def test_change_ratio():
    a = Image.new("RGB", (320, 180), "white")
    b = a.copy()
    assert change_ratio(a, b) == 0
    b.paste((0, 0, 0), (0, 0, 160, 180))
    assert 0.4 < change_ratio(a, b) < 0.6


def test_privacy_rules():
    rules = {"blocked_apps": ["keepassxc"], "blocked_titles": ["Private Browsing"]}
    assert is_private(WindowInfo(title="db.kdbx", app="org.keepassxc.KeePassXC"), rules)
    assert is_private(WindowInfo(title="Mozilla Firefox Private Browsing", app="firefox"), rules)
    assert not is_private(WindowInfo(title="Inbox", app="thunderbird"), rules)


def test_scenes_render():
    for name, (render, _, _) in SCENES.items():
        assert render().size == (1600, 900), name


def test_mock_tools_share_real_specs():
    reg = Mocks().registry()
    assert {"insert_text", "search_notes", "recent_emails", "delegate",
            "calendar__todays_events"} <= set(reg.tools)


def test_reply_checks_score_a_good_and_bad_trace():
    task = next(t for t in AGENT_TASKS if t.name == "reply_insert")
    good, bad = Mocks(), Mocks()
    good.inserted = ["Hi Dana, Thursday at 3pm works for me. Thanks, Alex"]
    bad.inserted = ["Here is a reply: Tuesday works."]
    ok = AgentResult("Inserted.", True, [ToolCall("insert_text", {}, "inserted")])
    assert all(fn(ok, good) for fn in task.checks.values())
    assert not all(fn(ok, bad) for fn in task.checks.values())


def test_mock_insert_records_text():
    m = Mocks()
    reg = m.registry()
    out = asyncio.run(reg.call("insert_text", {"text": "hello"}))
    assert out.startswith("inserted") and m.inserted == ["hello"]
