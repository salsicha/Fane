"""GNOME Online Accounts (Settings → Online Accounts): the Google sign-in built into Ubuntu.

Fane never stores Google credentials. It asks GOA for a short-lived OAuth token for the
one account the user selected, and only that account.
"""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass

from dbus_next import Message, MessageType
from dbus_next.aio import MessageBus

log = logging.getLogger(__name__)

GOA = "org.gnome.OnlineAccounts"
GOA_PATH = "/org/gnome/OnlineAccounts"


class AccountError(RuntimeError):
    pass


@dataclass
class GoaAccount:
    path: str
    provider: str
    email: str
    name: str
    mail_disabled: bool
    attention_needed: bool
    imap_host: str = "imap.gmail.com"
    smtp_host: str = "smtp.gmail.com"


def _v(props: dict, key: str, default=None):
    return props[key].value if key in props else default


class OnlineAccounts:
    def __init__(self, bus: MessageBus | None = None):
        self._bus = bus
        self._tokens: dict[str, tuple[str, float]] = {}

    async def _conn(self) -> MessageBus:
        if self._bus is None:
            self._bus = await MessageBus().connect()
        return self._bus

    async def _call(self, path: str, interface: str, member: str, signature: str = "", body=None):
        bus = await self._conn()
        reply = await bus.call(Message(destination=GOA, path=path, interface=interface, member=member,
                                       signature=signature, body=body or []))
        if reply.message_type == MessageType.ERROR:
            raise AccountError(f"{member}: {reply.error_name} {reply.body[0] if reply.body else ''}")
        return reply.body

    async def google_accounts(self) -> list[GoaAccount]:
        """Google accounts in Online Accounts that expose mail."""
        try:
            (objects,) = await self._call(GOA_PATH, "org.freedesktop.DBus.ObjectManager", "GetManagedObjects")
        except Exception as e:
            raise AccountError(f"GNOME Online Accounts unavailable: {e}") from e
        accounts = []
        for path, ifaces in objects.items():
            acc = ifaces.get(f"{GOA}.Account")
            mail = ifaces.get(f"{GOA}.Mail")
            if not acc or _v(acc, "ProviderType") != "google" or not mail:
                continue
            accounts.append(GoaAccount(
                path=path, provider="google",
                email=_v(mail, "EmailAddress") or _v(acc, "PresentationIdentity", ""),
                name=_v(mail, "Name", ""),
                mail_disabled=bool(_v(acc, "MailDisabled", False)),
                attention_needed=bool(_v(acc, "AttentionNeeded", False)),
                imap_host=_v(mail, "ImapHost") or "imap.gmail.com",
                smtp_host=_v(mail, "SmtpHost") or "smtp.gmail.com"))
        return sorted(accounts, key=lambda a: a.email.lower())

    async def find(self, email: str) -> GoaAccount | None:
        email = email.strip().lower()
        for acc in await self.google_accounts():
            if acc.email.lower() == email:
                return acc
        return None

    async def access_token(self, acc: GoaAccount) -> str:
        cached = self._tokens.get(acc.path)
        if cached and cached[1] > time.time() + 60:
            return cached[0]
        if acc.mail_disabled:
            raise AccountError(f"Mail is switched off for {acc.email} in Settings → Online Accounts")
        try:
            await self._call(acc.path, f"{GOA}.Account", "EnsureCredentials")
            token, expires_in = await self._call(acc.path, f"{GOA}.OAuth2Based", "GetAccessToken")
        except AccountError as e:
            raise AccountError(f"{acc.email} needs attention in Settings → Online Accounts ({e})") from e
        self._tokens[acc.path] = (token, time.time() + (expires_in or 600))
        return token

    def forget(self, acc: GoaAccount) -> None:
        self._tokens.pop(acc.path, None)
