"""Gmail over IMAP/SMTP with XOAUTH2, using tokens from GNOME Online Accounts."""

from __future__ import annotations

import asyncio
import base64
import email
import email.policy
import imaplib
import re
import smtplib
from dataclasses import dataclass, field
from email.message import EmailMessage
from email.utils import formatdate, getaddresses, make_msgid, parseaddr, parsedate_to_datetime
from typing import Awaitable, Callable

TokenFn = Callable[[], Awaitable[str]]


class MailError(RuntimeError):
    pass


@dataclass
class MailMessage:
    uid: int
    message_id: str
    from_: str
    from_addr: str
    to: list[str]
    cc: list[str]
    subject: str
    date: str
    body: str
    seen: bool
    references: str = ""
    # Topmost Authentication-Results header, i.e. the one Gmail's own servers added.
    auth_results: str = ""
    raw_headers: dict[str, str] = field(default_factory=dict)

    def summary(self, snippet: int = 200) -> dict:
        return {"uid": self.uid, "from": self.from_, "subject": self.subject, "date": self.date,
                "unread": not self.seen, "snippet": re.sub(r"\s+", " ", self.body)[:snippet]}


def parse_message(uid: int, raw: bytes, flags: str = "") -> MailMessage:
    msg = email.message_from_bytes(raw, policy=email.policy.default)
    body_part = msg.get_body(preferencelist=("plain", "html"))
    body = ""
    if body_part is not None:
        try:
            body = body_part.get_content()
        except Exception:
            body = ""
        if body_part.get_content_subtype() == "html":
            body = re.sub(r"<(script|style)[^>]*>.*?</\1>", " ", body, flags=re.S | re.I)
            body = re.sub(r"<br\s*/?>|</p>", "\n", body, flags=re.I)
            body = re.sub(r"<[^>]+>", " ", body)
    body = re.sub(r"[ \t]+", " ", body).strip()
    try:
        date = parsedate_to_datetime(msg["date"]).isoformat() if msg["date"] else ""
    except (TypeError, ValueError):
        date = str(msg["date"] or "")
    auth = msg.get_all("Authentication-Results") or []
    name_addr = str(msg["from"] or "")
    return MailMessage(
        uid=uid, message_id=str(msg["message-id"] or ""), from_=name_addr,
        from_addr=parseaddr(name_addr)[1].lower(),
        to=[a for _, a in getaddresses([str(v) for v in msg.get_all("to") or []]) if a],
        cc=[a for _, a in getaddresses([str(v) for v in msg.get_all("cc") or []]) if a],
        subject=str(msg["subject"] or ""), date=date, body=body[:20000],
        seen="\\Seen" in flags, references=str(msg["references"] or ""),
        auth_results=str(auth[0]) if auth else "")


def strip_quoted(body: str) -> str:
    """Drop the quoted history under a reply ("On … wrote:" and "> " lines)."""
    out = []
    for line in body.splitlines():
        if re.match(r"^\s*On .{5,200} wrote:\s*$", line) or line.strip() == "-----Original Message-----":
            break
        if not line.lstrip().startswith(">"):
            out.append(line)
    return "\n".join(out).strip()


def build_message(sender: str, to: list[str], subject: str, body: str, cc: list[str] | None = None,
                  reply_to: MailMessage | None = None) -> EmailMessage:
    msg = EmailMessage()
    msg["From"] = sender
    msg["To"] = ", ".join(to)
    if cc:
        msg["Cc"] = ", ".join(cc)
    if reply_to is not None:
        if not subject:
            subject = reply_to.subject
        if not subject.lower().startswith("re:"):
            subject = f"Re: {subject}"
        if reply_to.message_id:
            msg["In-Reply-To"] = reply_to.message_id
            msg["References"] = f"{reply_to.references} {reply_to.message_id}".strip()
    msg["Subject"] = subject
    msg["Date"] = formatdate(localtime=True)
    msg["Message-ID"] = make_msgid(domain=sender.split("@")[-1])
    msg.set_content(body)
    return msg


def _xoauth2(user: str, token: str) -> str:
    return f"user={user}\x01auth=Bearer {token}\x01\x01"


class GmailClient:
    """One IMAP connection per operation: simple, and immune to token expiry mid-session."""

    def __init__(self, address: str, token: TokenFn, imap_host: str = "imap.gmail.com",
                 smtp_host: str = "smtp.gmail.com"):
        self.address = address
        self._token = token
        self.imap_host = imap_host
        self.smtp_host = smtp_host
        self._lock = asyncio.Lock()

    async def _imap(self, fn: Callable[[imaplib.IMAP4_SSL], object], readonly: bool = True):
        token = await self._token()

        def run():
            imap = imaplib.IMAP4_SSL(self.imap_host, timeout=30)
            try:
                imap.authenticate("XOAUTH2", lambda _: _xoauth2(self.address, token).encode())
                typ, _ = imap.select("INBOX", readonly=readonly)
                if typ != "OK":
                    raise MailError("cannot open INBOX")
                return fn(imap)
            finally:
                try:
                    imap.logout()
                except Exception:
                    pass

        async with self._lock:
            try:
                return await asyncio.to_thread(run)
            except imaplib.IMAP4.error as e:
                raise MailError(f"IMAP: {e}") from e

    @staticmethod
    def _fetch(imap: imaplib.IMAP4_SSL, uids: list[int]) -> list[MailMessage]:
        if not uids:
            return []
        typ, data = imap.uid("FETCH", ",".join(map(str, uids)), "(UID FLAGS BODY.PEEK[])")
        out = []
        for item in data or []:
            if not isinstance(item, tuple):
                continue
            meta = item[0].decode(errors="replace")
            m = re.search(r"UID (\d+)", meta)
            if m:
                flags = re.search(r"FLAGS \(([^)]*)\)", meta)
                out.append(parse_message(int(m.group(1)), item[1], flags.group(1) if flags else ""))
        return out

    @staticmethod
    def _search(imap: imaplib.IMAP4_SSL, *criteria: str) -> list[int]:
        typ, data = imap.uid("SEARCH", None, *criteria)
        if typ != "OK" or not data or not data[0]:
            return []
        return [int(x) for x in data[0].split()]

    async def state(self) -> tuple[int, int]:
        """(UIDVALIDITY, highest UID) of the inbox."""
        def fn(imap):
            validity = int((imap.response("UIDVALIDITY")[1] or [b"0"])[0])
            uids = self._search(imap, "ALL")
            return validity, max(uids, default=0)
        return await self._imap(fn)

    async def new_since(self, last_uid: int) -> tuple[int, list[MailMessage]]:
        def fn(imap):
            validity = int((imap.response("UIDVALIDITY")[1] or [b"0"])[0])
            # "N:*" always returns the last message, even if it's older; filter below.
            uids = [u for u in self._search(imap, "UID", f"{last_uid + 1}:*") if u > last_uid]
            return validity, self._fetch(imap, uids[:50])
        return await self._imap(fn)

    async def inbox(self, limit: int = 10, query: str = "", unread_only: bool = False) -> list[MailMessage]:
        def fn(imap):
            criteria = ["UNSEEN"] if unread_only else ["ALL"]
            if query:
                criteria = criteria[:1] + ["X-GM-RAW", '"' + query.replace('"', "") + '"']
            uids = self._search(imap, *criteria)[-max(1, min(int(limit), 50)):]
            return sorted(self._fetch(imap, uids), key=lambda m: m.uid, reverse=True)
        return await self._imap(fn)

    async def get(self, uid: int) -> MailMessage | None:
        msgs = await self._imap(lambda imap: self._fetch(imap, [int(uid)]))
        return msgs[0] if msgs else None

    async def mark_seen(self, uid: int) -> None:
        await self._imap(lambda imap: imap.uid("STORE", str(uid), "+FLAGS", "(\\Seen)"), readonly=False)

    async def send(self, msg: EmailMessage) -> str:
        token = await self._token()

        def run():
            with smtplib.SMTP(self.smtp_host, 587, timeout=30) as smtp:
                smtp.ehlo()
                smtp.starttls()
                smtp.ehlo()
                auth = base64.b64encode(_xoauth2(self.address, token).encode()).decode()
                code, resp = smtp.docmd("AUTH", "XOAUTH2 " + auth)
                if code != 235:
                    raise MailError(f"SMTP auth failed ({code}): {resp[:200]!r}")
                smtp.send_message(msg)

        async with self._lock:
            try:
                await asyncio.to_thread(run)
            except (smtplib.SMTPException, OSError) as e:
                raise MailError(f"SMTP: {e}") from e
        return str(msg["Message-ID"])
