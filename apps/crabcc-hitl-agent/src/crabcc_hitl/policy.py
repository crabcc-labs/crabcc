"""Per-argument auto-approve policy for the tool gate.

Phase 2 had a binary distinction: a tool either always asks for human
approval (``required``) or always runs (``auto``). Phase 3 adds a
middle layer — a *required* tool can still skip the prompt when its
arguments match an explicit allowlist.

Policy syntax (env var ``CRABCC_HITL_APPROVAL_AUTO_PATTERNS``):

* Comma-separated rules.
* Each rule has the shape ``tool:arg=glob``.
* ``glob`` follows :mod:`fnmatch` semantics — ``*`` matches any
  substring, ``?`` matches a single character. Stays in stdlib so
  there's no regex dependency / DoS surface.
* Multiple rules for the same tool are OR'd (any matching rule
  auto-approves).

Examples::

    fetch_url:url=https://github.com/**
    fetch_url:url=https://docs.python.org/*
    memory_remember:key=note:*

Failure modes are explicit — a malformed rule is logged and skipped
rather than throwing at startup, because settings load once and a
single typo would otherwise wedge the entire service.
"""

from __future__ import annotations

import fnmatch
import logging
from dataclasses import dataclass, field
from typing import Any

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class _Rule:
    """A single ``tool:arg=glob`` allowlist entry."""

    tool: str
    arg: str
    pattern: str

    def matches(self, arguments: dict[str, Any]) -> bool:
        """``True`` when ``arguments[self.arg]`` matches ``self.pattern``.

        Missing args don't match. Non-string args are coerced via
        ``str()`` so an integer arg can still be globbed (rare but
        harmless).
        """
        value = arguments.get(self.arg)
        if value is None:
            return False
        return fnmatch.fnmatchcase(str(value), self.pattern)


@dataclass
class ApprovalPolicy:
    """Allowlist of tool-arg patterns that bypass the human prompt.

    Construct via :meth:`from_env_value` to absorb a comma-separated
    ``tool:arg=glob`` env string. Use :meth:`auto_approves` from the
    tool gate to short-circuit before queueing a prompt.
    """

    rules_by_tool: dict[str, list[_Rule]] = field(default_factory=dict)

    @classmethod
    def from_env_value(cls, raw: str | None) -> ApprovalPolicy:
        """Parse the ``CRABCC_HITL_APPROVAL_AUTO_PATTERNS`` env shape.

        ``None`` / empty string yields an empty policy (zero rules).
        Malformed rules are logged and skipped — startup must not
        fail on a single typo.
        """
        if not raw:
            return cls()
        rules_by_tool: dict[str, list[_Rule]] = {}
        for token in raw.split(","):
            rule_text = token.strip()
            if not rule_text:
                continue
            rule = _parse_rule(rule_text)
            if rule is None:
                continue
            rules_by_tool.setdefault(rule.tool, []).append(rule)
        return cls(rules_by_tool=rules_by_tool)

    def __bool__(self) -> bool:
        return bool(self.rules_by_tool)

    def auto_approves(self, *, tool: str, arguments: dict[str, Any]) -> _Rule | None:
        """Return the first matching rule, or ``None`` to keep prompting.

        Returning the rule (vs a bool) lets the caller log *which*
        pattern fired — useful in the audit trail for explaining
        why a sensitive tool ran without human input.
        """
        for rule in self.rules_by_tool.get(tool, ()):
            if rule.matches(arguments):
                return rule
        return None


def _parse_rule(text: str) -> _Rule | None:
    """Parse one ``tool:arg=glob`` token. Returns ``None`` on failure."""
    if ":" not in text or "=" not in text:
        logger.warning("approval policy: skipping malformed rule %r", text)
        return None
    tool, rest = text.split(":", 1)
    arg, _, pattern = rest.partition("=")
    tool = tool.strip()
    arg = arg.strip()
    pattern = pattern.strip()
    if not tool or not arg or not pattern:
        logger.warning("approval policy: skipping incomplete rule %r", text)
        return None
    return _Rule(tool=tool, arg=arg, pattern=pattern)
