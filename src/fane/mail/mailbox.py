"""Fane's own mailbox: watch for new mail, answer the user's emailed requests, and give
the agent tools to read and send mail as itself."""

from __future__ import annotations

import asyncio
import json
import logging
import time
from collections import deque
from typing import Any, Awaitable, Protocol

from fane.config import STATE_DIR
from fane.connectors import Tool, params
from fane.mail.gmail import GmailClient, MailError, MailMessage, build_message, strip_quoted
from fane.mail.trust import untrusted_block, verified_trusted

log = logging.getLogger(__name__)

REMOTE_PROMPT = """\
The user is away from the computer and emailed you at your own address ({me}).
Do what they ask using your tools, then write your reply to them as your final answer.
It is sent as the email reply: plain text, no preamble, don't mention tools.

From: {sender}
Subject: {subject}

{body}"""


class MailHost(Protocol):
    """What the mailbox needs from the rest of Fane."""

    async def run_remote(self, prompt: str) -> str: ...
    def show_mail(self, info: dict[str, Any]) -> None: ...
    def notify(self, text: str) -> None: ...
    def confirm_send(self, details: dict[str, Any]) -> Awaitable[bool]: ...


class AgentMailbox:
    def __init__(self, client: GmailClient, cfg: dict[str, Any], host: MailHost):
        self.client = client
        self.cfg = cfg
        self.host = host
        self.address = client.address
        self._state_path = STATE_DIR / f"mail-{self.address}.json"
        self._validity = 0
        self._last_uid = 0
        self._sent: deque[float] = deque()
        self._task: asyncio.Task | None = None
        self.error: str | None = None

    @property
    def trusted(self) -> list[str]:
        return list(self.cfg.get("trusted_senders", []))

    # --- lifecycle ------------------------------------------------------------------
    async def start(self) -> None:
        try:
            saved = json.loads(self._state_path.read_text())
            self._validity, self._last_uid = saved["uidvalidity"], saved["last_uid"]
        except (OSError, ValueError, KeyError):
            # First run: only react to mail that arrives from now on.
            self._validity, self._last_uid = await self.client.state()
            self._save()
        self._task = asyncio.create_task(self._loop(), name=f"mail:{self.address}")
        log.info("watching %s (from UID %d)", self.address, self._last_uid)

    async def stop(self) -> None:
        if self._task:
            self._task.cancel()

    def _save(self) -> None:
        self._state_path.parent.mkdir(parents=True, exist_ok=True)
        self._state_path.write_text(json.dumps({"uidvalidity": self._validity, "last_uid": self._last_uid}))

    async def _loop(self) -> None:
        delay = self.cfg["poll_seconds"]
        while True:
            try:
                await self.poll()
                self.error, delay = None, self.cfg["poll_seconds"]
            except asyncio.CancelledError:
                raise
            except Exception as e:
                if self.error != str(e):
                    log.warning("mail poll failed: %s", e)
                self.error = str(e)
                delay = min(delay * 2, 900)  # back off while offline / token problems
            await asyncio.sleep(delay)

    async def poll(self) -> None:
        validity, msgs = await self.client.new_since(self._last_uid)
        if validity != self._validity:
            # Mailbox was rebuilt; UIDs restarted. Don't replay old mail.
            self._validity, self._last_uid = await self.client.state()
            self._save()
            return
        for msg in sorted(msgs, key=lambda m: m.uid):
            self._last_uid = max(self._last_uid, msg.uid)
            self._save()
            try:
                await self.handle(msg)
            except Exception:
                log.exception("handling mail %s failed", msg.uid)

    # --- incoming ------------------------------------------------------------------------
    async def handle(self, msg: MailMessage) -> None:
        if msg.from_addr == self.address.lower():
            return
        sender = verified_trusted(msg, self.trusted)
        if sender and self.cfg["auto_reply_trusted"]:
            if self._rate_ok():
                await self._answer(msg, sender)
                return
            self.host.notify(f"Auto-reply limit reached; not answering {sender} automatically.")
        self.host.show_mail({"uid": msg.uid, "from": msg.from_, "subject": msg.subject,
                             "snippet": msg.body[:300], "trusted": bool(sender)})

    async def _answer(self, msg: MailMessage, sender: str) -> None:
        log.info("emailed request from %s: %s", sender, msg.subject)
        prompt = REMOTE_PROMPT.format(me=self.address, sender=sender, subject=msg.subject,
                                      body=strip_quoted(msg.body)[:6000])
        try:
            reply = await self.host.run_remote(prompt)
        except Exception as e:
            reply = f"Sorry, I couldn't do that: {e}"
        body = (reply.strip() or "Done.") + f"\n\n{self.cfg['signature']}"
        await self.client.send(build_message(self.address, [sender], "", body, reply_to=msg))
        self._sent.append(time.time())
        await self.client.mark_seen(msg.uid)
        self.host.notify(f"Replied to {sender}: {msg.subject[:60]}")

    def _rate_ok(self) -> bool:
        now = time.time()
        while self._sent and now - self._sent[0] > 3600:
            self._sent.popleft()
        return len(self._sent) < self.cfg["max_auto_replies_per_hour"]

    # --- outgoing ---------------------------------------------------------------------------
    async def _send_checked(self, to: list[str], subject: str, body: str, cc: list[str],
                            reply_to: MailMessage | None) -> str:
        recipients = [a.strip() for a in to + cc if a.strip()]
        if not recipients:
            return "error: no recipient"
        trusted = {t.lower() for t in self.trusted}
        if not all(r.lower() in trusted for r in recipients):
            ok = await self.host.confirm_send({"from": self.address, "to": to, "cc": cc,
                                               "subject": subject or (reply_to.subject if reply_to else ""),
                                               "body": body})
            if not ok:
                return "not sent: the user declined (or didn't confirm in time)"
        msg = build_message(self.address, to, subject, body + f"\n\n{self.cfg['signature']}", cc, reply_to)
        try:
            await self.client.send(msg)
        except MailError as e:
            return f"error: {e}"
        return f"sent to {', '.join(recipients)}"

    # --- agent tools ------------------------------------------------------------------------
    def tools(self) -> list[Tool]:
        me = self.address
        S = lambda t, d="": {"type": t, "description": d}  # noqa: E731

        async def inbox(limit: int = 10, query: str = "", unread_only: bool = False) -> Any:
            msgs = await self.client.inbox(int(limit or 10), query, bool(unread_only))
            out = []
            for m in msgs:
                item = m.summary()
                item["verified_trusted_sender"] = bool(verified_trusted(m, self.trusted))
                if not item["verified_trusted_sender"]:
                    item["snippet"] = untrusted_block(item["snippet"], m.from_addr)
                out.append(item)
            return out or "Inbox is empty."

        async def read(uid: int) -> str:
            m = await self.client.get(int(uid))
            if m is None:
                return "error: no such message"
            head = f"From: {m.from_}\nTo: {', '.join(m.to)}\nSubject: {m.subject}\nDate: {m.date}\n"
            body = m.body if verified_trusted(m, self.trusted) else untrusted_block(m.body, m.from_addr)
            return head + "\n" + body

        async def send(to: Any = "", subject: str = "", body: str = "", cc: Any = "") -> str:
            split = lambda v: [a.strip() for a in (v if isinstance(v, list) else str(v).split(",")) if a.strip()]  # noqa: E731
            return await self._send_checked(split(to), subject, body, split(cc), None)

        async def reply(uid: int, body: str = "") -> str:
            m = await self.client.get(int(uid))
            if m is None:
                return "error: no such message"
            return await self._send_checked([m.from_addr], "", body, [], m)

        return [
            Tool("fane_inbox", f"List mail in YOUR OWN mailbox ({me}), newest first. This is Fane's "
                 "account, not the user's mail.",
                 params([], limit=S("integer"), query=S("string", "Gmail search, e.g. from:dana"),
                        unread_only=S("boolean")), inbox, "Checking my inbox"),
            Tool("fane_read_email", f"Read one email in your own mailbox ({me}) by uid.",
                 params(["uid"], uid=S("integer")), read, "Reading email"),
            Tool("fane_send_email", f"Send an email from your own address ({me}). The user confirms "
                 "before it goes to anyone they haven't marked as trusted.",
                 params(["to", "subject", "body"], to=S("string", "Comma-separated addresses"),
                        subject=S("string"), body=S("string", "Plain-text body, no signature"),
                        cc=S("string")), send, "Sending email"),
            Tool("fane_reply_email", f"Reply to an email in your own mailbox ({me}).",
                 params(["uid", "body"], uid=S("integer"), body=S("string", "Plain-text reply")),
                 reply, "Replying"),
        ]


def mail_tool_names() -> set[str]:
    return {"fane_inbox", "fane_read_email", "fane_send_email", "fane_reply_email"}

