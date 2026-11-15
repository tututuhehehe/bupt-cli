"""JSON output helpers."""

from __future__ import annotations

import json
import sys
from typing import Any, TextIO


def emit(payload: Any, pretty: bool = False, stream: TextIO | None = None) -> None:
    """Write one JSON document to stdout (compact by default)."""
    out = stream or sys.stdout
    if pretty:
        json.dump(payload, out, ensure_ascii=False, indent=2, sort_keys=False)
    else:
        json.dump(
            payload,
            out,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=False,
        )
    out.write("\n")
    out.flush()


def emit_error(payload: Any, pretty: bool = False) -> None:
    """Write an error envelope to stderr."""
    emit(payload, pretty=pretty, stream=sys.stderr)
