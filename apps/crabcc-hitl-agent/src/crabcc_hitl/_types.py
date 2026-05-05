"""Wire-shape and protocol types shared across the package.

Centralises every ``TypedDict`` / ``Protocol`` we expose so individual
modules stay focused on behaviour. Mypy strict catches drift between
producers (e.g. ``tool_gate._build_keyboard``) and consumers (e.g.
``TelegramBotClient.send_message``) when both reference the same name
here.

Naming convention: TypedDicts that mirror a Telegram REST request
body get a ``Body`` suffix (``SendMessageBody``); response envelopes
end in ``Envelope``; reusable wire fragments use the protocol's own
noun (``InlineKeyboardMarkup``).
"""

from __future__ import annotations

from typing import NotRequired, TypedDict

# ───── Telegram inline keyboard ─────


class InlineKeyboardButton(TypedDict, total=False):
    """A single inline keyboard button.

    ``text`` is required at the wire level. ``callback_data`` /
    ``url`` / ``web_app`` are mutually exclusive — constructors set
    exactly one. Marked ``total=False`` so Pyright/mypy don't trip
    on the partial population.
    """

    text: str
    callback_data: str
    url: str


class InlineKeyboardMarkup(TypedDict):
    inline_keyboard: list[list[InlineKeyboardButton]]


# ───── Telegram REST request bodies ─────


class SendMessageBody(TypedDict):
    chat_id: int
    text: str
    reply_markup: NotRequired[InlineKeyboardMarkup]
    parse_mode: NotRequired[str]


class EditMessageTextBody(TypedDict):
    chat_id: int
    message_id: int
    text: str
    reply_markup: NotRequired[InlineKeyboardMarkup]
    parse_mode: NotRequired[str]


class AnswerCallbackQueryBody(TypedDict):
    callback_query_id: str
    text: NotRequired[str]


# ───── Telegram REST response envelope ─────


class TelegramEnvelope(TypedDict, total=False):
    """Telegram REST envelope.

    Every method returns ``ok`` plus either ``result`` or
    ``description`` + ``error_code``. We don't model per-method
    ``result`` shapes because the service discards the contents —
    surfacing failure (``ok: false``) is the only branch that
    matters.
    """

    ok: bool
    description: str
    error_code: int
