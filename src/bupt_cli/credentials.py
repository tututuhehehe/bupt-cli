"""Credential loading for the ``bupt`` CLI.

Precedence:
  1. environment: BUPT_USERNAME / BUPT_PASSWORD
  2. ``~/.bupt-cli/credentials.json`` (created by `bupt login`), unless
     BUPT_CLI_CONFIG_DIR overrides the directory.

``role`` records whether the account belongs to the graduate or undergraduate
academic system, which decides whether `bupt grad ...` or the undergraduate
JWGL commands apply.

Values are never logged or included in command output. `bupt login` prompts for
them; the `--password` flag exists for scripting but puts the secret in your
shell history and the process list, so the prompt is preferred.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from bupt_cli.errors import AuthError

CONFIG_DIR_ENV = "BUPT_CLI_CONFIG_DIR"
DEFAULT_CONFIG_DIR = "~/.bupt-cli"
CONFIG_FILENAME = "credentials.json"


def config_dir() -> Path:
    """Resolve the config directory (see the module docstring for precedence)."""
    override = os.environ.get(CONFIG_DIR_ENV)
    if override:
        return Path(override).expanduser()
    return Path(DEFAULT_CONFIG_DIR).expanduser()


def credentials_path() -> Path:
    return config_dir() / CONFIG_FILENAME


@dataclass(frozen=True)
class Credentials:
    """One BUPT account. BUPT uses 统一身份认证 for every system, so the
    academic (JWGL) systems accept the same credentials as CAS."""

    username: str
    password: str
    # "graduate" | "undergraduate" | "" (unknown). Decides which academic
    # system the account belongs to.
    role: str = ""

    def secret_values(self) -> list[str]:
        """Values that must never appear in output."""
        return [self.password]


def _read_config() -> dict:
    path = credentials_path()
    if not path.exists():
        return {}
    try:
        with path.open("r", encoding="utf-8") as handle:
            data = json.load(handle)
    except (OSError, ValueError) as exc:
        raise AuthError(
            f"cannot read credentials file {path}",
            hint=f"fix the file permissions/content ({exc.__class__.__name__})",
        ) from None
    if not isinstance(data, dict):
        raise AuthError(f"credentials file {path} must contain a JSON object")
    return data


ROLES = ("graduate", "undergraduate")
ROLE_LABELS = {"graduate": "研究生", "undergraduate": "本科生"}


def normalize_role(value: Any) -> str:
    """Map user input to a canonical role, or "" when unknown."""
    text = str(value or "").strip().lower()
    if text in ROLES:
        return text
    if text in ("研究生", "硕士", "博士", "grad", "postgrad"):
        return "graduate"
    if text in ("本科生", "本科", "undergrad", "ug"):
        return "undergraduate"
    return ""


def save_credentials(username: str, password: str, role: str = "") -> Path:
    """Persist credentials to the config dir with mode 0600.

    Merges into any file already present instead of replacing it, so keys this
    version does not know about (future options, hand-written extras) survive a
    re-login.
    """
    path = credentials_path()
    path.parent.mkdir(parents=True, exist_ok=True)

    # Start from what is on disk so unrelated keys survive.
    try:
        payload: dict[str, Any] = dict(_read_config())
    except AuthError:
        payload = {}

    payload["username"] = username
    payload["password"] = password
    if role:
        payload["role"] = role

    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    try:
        path.chmod(0o600)
    except OSError:
        pass
    return path


def load_credentials(required: bool = True) -> Credentials:
    """Load credentials from env/config without printing any secret."""
    config = _read_config()

    username = os.environ.get("BUPT_USERNAME") or str(config.get("username") or "")
    password = os.environ.get("BUPT_PASSWORD") or str(config.get("password") or "")
    username = username.strip()

    if not username or not password:
        if not required:
            return Credentials("", "")
        raise AuthError(
            "BUPT credentials are not configured",
            hint="set BUPT_USERNAME and BUPT_PASSWORD, or create "
            f"{credentials_path()} with keys username/password",
        )

    role = normalize_role(
        os.environ.get("BUPT_ROLE") or config.get("role")
    )

    return Credentials(username=username, password=password, role=role)
