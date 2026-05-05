"""Minimal Telegram Bot API client + Mini App initData validator.

This service does NOT consume ``getUpdates`` — that's the Rust
``crabcc-telegram`` bot's job. We only emit messages (sendMessage,
editMessageText, answerCallbackQuery) and validate Mini App ``initData``
HMAC signatures coming from the WebApp.

Both halves live in this module because they share the same bot
token + the same httpx client.
"""

from __future__ import annotations

import hashlib
import hmac
import logging
import urllib.parse
from collections.abc import Mapping
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import httpx

    from ._types import (
        AnswerCallbackQueryBody,
        EditMessageTextBody,
        InlineKeyboardMarkup,
        SendMessageBody,
        TelegramEnvelope,
    )

logger = logging.getLogger(__name__)


class TelegramBotClient:
    """Single-method-set Telegram REST client.

    Wraps the few endpoints HITL needs to send approval prompts and
    acknowledge inline-button taps. We share the service-wide httpx
    pool to keep one warm connection to ``api.telegram.org``.
    """

    BASE = "https://api.telegram.org"

    def __init__(self, token: str, http: httpx.AsyncClient) -> None:
        self._token = token
        self._http = http

    async def send_message(
        self,
        *,
        chat_id: int,
        text: str,
        reply_markup: InlineKeyboardMarkup | None = None,
        parse_mode: str | None = None,
    ) -> None:
        """POST /sendMessage. Raises :class:`TelegramApiError` on rejection.

        Return value is discarded — every caller in this service only
        cares whether the message went out, not the resulting Message
        object. Keeping the return tight (``None``) means callers can't
        accidentally depend on Telegram's wire shape.
        """
        body: SendMessageBody = {"chat_id": chat_id, "text": text}
        if reply_markup is not None:
            body["reply_markup"] = reply_markup
        if parse_mode is not None:
            body["parse_mode"] = parse_mode
        await self._call("sendMessage", body)

    async def edit_message_text(
        self,
        *,
        chat_id: int,
        message_id: int,
        text: str,
        reply_markup: InlineKeyboardMarkup | None = None,
        parse_mode: str | None = None,
    ) -> None:
        body: EditMessageTextBody = {
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
        }
        if reply_markup is not None:
            body["reply_markup"] = reply_markup
        if parse_mode is not None:
            body["parse_mode"] = parse_mode
        await self._call("editMessageText", body)

    async def answer_callback_query(
        self,
        *,
        callback_query_id: str,
        text: str | None = None,
    ) -> None:
        body: AnswerCallbackQueryBody = {"callback_query_id": callback_query_id}
        if text is not None:
            body["text"] = text
        await self._call("answerCallbackQuery", body)

    async def _call(self, method: str, body: Mapping[str, object]) -> None:
        url = f"{self.BASE}/bot{self._token}/{method}"
        resp = await self._http.post(url, json=body)
        # Telegram returns 200 with ``ok: false`` for logical errors;
        # ``raise_for_status`` covers the rare DNS / transport blip.
        resp.raise_for_status()
        envelope: TelegramEnvelope = resp.json()
        if not envelope.get("ok"):
            raise TelegramApiError(method=method, envelope=envelope)


class TelegramApiError(RuntimeError):
    """Telegram returned ``ok: false`` for a method call."""

    def __init__(self, *, method: str, envelope: TelegramEnvelope) -> None:
        description = envelope.get("description", "no description")
        super().__init__(f"telegram {method}: {description}")
        self.method = method
        self.envelope = envelope


# ───── Mini App initData validation ─────


def validate_init_data(init_data: str, bot_token: str) -> dict[str, str] | None:
    """Verify a Mini App ``initData`` payload's HMAC signature.

    Telegram emits ``initData`` as the ``window.Telegram.WebApp.initData``
    string — a URL-encoded ``key=value&key=value`` blob with a ``hash``
    field appended. The signature is HMAC-SHA256 over the sorted, joined
    pairs minus ``hash``, keyed by HMAC-SHA256("WebAppData", bot_token).
    Spec: https://core.telegram.org/bots/webapps#validating-data-received-via-the-mini-app

    Args:
        init_data: The raw ``initData`` string from the Mini App.
        bot_token: The bot's API token (same one the Rust bot uses).

    Returns:
        Parsed ``dict[str, str]`` of fields when the signature checks
        out, else ``None``. Caller should reject the request on
        ``None`` rather than treating fields as trusted.
    """
    if not init_data:
        return None
    pairs = urllib.parse.parse_qsl(init_data, keep_blank_values=True)
    fields = dict(pairs)
    received_hash = fields.pop("hash", None)
    if received_hash is None:
        return None
    # Build the data-check string: sorted "k=v" pairs joined by \n.
    data_check = "\n".join(f"{k}={v}" for k, v in sorted(fields.items()))
    secret_key = hmac.new(b"WebAppData", bot_token.encode(), hashlib.sha256).digest()
    expected_hash = hmac.new(secret_key, data_check.encode(), hashlib.sha256).hexdigest()
    if not hmac.compare_digest(expected_hash, received_hash):
        logger.info("init_data signature mismatch")
        return None
    return fields
