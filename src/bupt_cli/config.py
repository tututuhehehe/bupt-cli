"""Optional non-secret CLI configuration (dorm defaults).

The dorm section lives in ``config.json`` next to ``credentials.json`` (in
``~/.bupt-cli`` unless ``BUPT_CLI_CONFIG_DIR`` overrides it):

.. code-block:: json

    {"dorm": {"building": "学一楼", "room": "101", "floor": "1"}}

The file is optional. Invalid JSON is a usage error (exit code 2) so a typo
never silently changes which meter is queried.
"""

from __future__ import annotations

import json
from pathlib import Path

from bupt_cli.credentials import config_dir
from bupt_cli.errors import UsageError

DORM_CONFIG_FILE = "config.json"
DORM_KEYS = ("building", "room", "floor")


def dorm_config_path() -> Path:
    return config_dir() / DORM_CONFIG_FILE


def load_dorm_config() -> dict[str, str]:
    """Return the ``dorm`` section of config.json (empty when absent)."""
    path = dorm_config_path()
    if not path.exists():
        return {}
    try:
        with path.open("r", encoding="utf-8") as handle:
            data = json.load(handle)
    except OSError as exc:
        raise UsageError(
            f"cannot read config file {path}",
            hint=f"check file permissions/readability ({exc.__class__.__name__})",
        ) from None
    except ValueError as exc:
        raise UsageError(
            f"cannot parse config file {path}",
            hint=f"fix the JSON content ({exc.__class__.__name__})",
        ) from None
    if not isinstance(data, dict):
        raise UsageError(f"config file {path} must contain a JSON object")
    dorm = data.get("dorm")
    if dorm is None:
        return {}
    if not isinstance(dorm, dict):
        raise UsageError(f"config file {path}: 'dorm' must be a JSON object")
    resolved: dict[str, str] = {}
    for key in DORM_KEYS:
        value = dorm.get(key)
        if value is None:
            continue
        text = str(value).strip()
        if text:
            resolved[key] = text
    return resolved
