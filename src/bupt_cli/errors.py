"""Error types and exit-code mapping for the ``bupt`` CLI.

Exit codes (shared with the JSON error envelope):
    2 usage, 3 auth, 4 network, 5 parse/upstream,
    6 submit not verified, 7 verification error.
"""

from __future__ import annotations

import re
from typing import Optional

from bupt_cli._vendor.campus_api.exceptions import (
    CampusAuthError,
    CampusError,
    CampusNetworkError,
    CampusParseError,
    CampusResourceNotFoundError,
)

EXIT_USAGE = 2
EXIT_AUTH = 3
EXIT_NETWORK = 4
EXIT_UPSTREAM = 5
EXIT_SUBMIT_UNVERIFIED = 6
EXIT_VERIFY_ERROR = 7


class CliError(Exception):
    """Base class for failures that should become a JSON error envelope."""

    error_type = "upstream"
    exit_code = EXIT_UPSTREAM

    def __init__(self, message: str, hint: str = ""):
        super().__init__(message)
        self.message = message
        self.hint = hint

    def as_dict(self) -> dict:
        error: dict[str, str] = {"type": self.error_type, "message": self.message}
        if self.hint:
            error["hint"] = self.hint
        return {"ok": False, "error": error}


class UsageError(CliError):
    error_type = "usage"
    exit_code = EXIT_USAGE


class AuthError(CliError):
    error_type = "auth"
    exit_code = EXIT_AUTH


class NetworkError(CliError):
    error_type = "network"
    exit_code = EXIT_NETWORK


class UpstreamError(CliError):
    error_type = "upstream"
    exit_code = EXIT_UPSTREAM


class ParseError(UpstreamError):
    error_type = "parse"


class NotFoundError(UpstreamError):
    error_type = "not_found"


def from_campus_error(exc: CampusError) -> CliError:
    """Map a vendored SDK exception to a CLI error with the right exit code."""
    message = str(exc)
    if isinstance(exc, CampusAuthError):
        from bupt_cli.credentials import credentials_path

        return AuthError(
            message,
            hint="run `bupt login` (credentials come from BUPT_USERNAME/BUPT_PASSWORD "
            f"or {credentials_path()})",
        )
    if isinstance(exc, CampusNetworkError):
        # HTTP status failures are upstream problems, not transport errors.
        if re.search(r"(?:Client|Server) error '[45]\d\d", message):
            if re.search(r"(?:401|403) (?:Unauthorized|Forbidden)", message):
                return AuthError(message)
            return UpstreamError(message)
        return NetworkError(message)
    if isinstance(exc, CampusResourceNotFoundError):
        return NotFoundError(message)
    if isinstance(exc, CampusParseError):
        return ParseError(message)
    return UpstreamError(message)


def redact(text: str, secrets: list[str]) -> str:
    """Remove known secret values from an arbitrary string.

    Only non-empty values are considered. This is a last line of defence:
    code paths are expected not to format secrets in the first place.
    """
    if not text:
        return text
    cleaned = text
    for secret in secrets:
        if secret and len(secret) >= 4:
            cleaned = cleaned.replace(secret, "[REDACTED]")
    return cleaned


def error_envelope(exc: BaseException, secrets: Optional[list[str]] = None) -> tuple[dict, int]:
    """Normalize any exception into (JSON envelope, exit code)."""
    secret_values = secrets or []
    if isinstance(exc, CliError):
        envelope = exc.as_dict()
        envelope["error"]["message"] = redact(envelope["error"]["message"], secret_values)
        if "hint" in envelope["error"]:
            envelope["error"]["hint"] = redact(envelope["error"]["hint"], secret_values)
        return envelope, exc.exit_code

    if isinstance(exc, CampusError):
        cli_exc = from_campus_error(exc)
        return error_envelope(cli_exc, secret_values)

    import httpx

    if isinstance(exc, httpx.TimeoutException):
        return (
            {
                "ok": False,
                "error": {
                    "type": "network",
                    "message": redact(f"request timed out: {exc}", secret_values),
                },
            },
            EXIT_NETWORK,
        )
    if isinstance(exc, httpx.HTTPError):
        return (
            {
                "ok": False,
                "error": {
                    "type": "network",
                    "message": redact(f"HTTP transport error: {exc}", secret_values),
                },
            },
            EXIT_NETWORK,
        )

    return (
        {
            "ok": False,
            "error": {
                "type": "upstream",
                "message": redact(f"unexpected error: {exc}", secret_values),
            },
        },
        EXIT_UPSTREAM,
    )
