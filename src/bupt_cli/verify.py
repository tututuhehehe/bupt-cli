"""Read-only post-submit verification.

`POST /ykt-site/work/submit` returning ``success`` is not enough: the write may
fail silently or land with stale content. This module performs read-only checks
against the submit-view endpoint and the per-course student list and reports
whether the submission is really there.

Everything here is pure: the two upstream calls and the sleep function are
injected, so the algorithm is unit-testable without network access. This module
must never call a write endpoint.
"""

from __future__ import annotations

import time
from typing import Any, Callable, Optional

from bupt_cli._vendor.campus_api.bupt.models import html_to_text
from bupt_cli.errors import redact

MANUAL_CHECK_HINT = (
    "upstream reported success but the submission could not be confirmed; "
    "check manually with `bupt homework show ID` or on ucloud.bupt.edu.cn. "
    "Do NOT resubmit blindly — every submit overwrites the previous one."
)

# `homework verify` never submits, so it must not reuse the "upstream reported
# success" wording of the post-submit path.
VERIFY_ONLY_HINT = (
    "the submission could not be confirmed; check manually with "
    "`bupt homework show ID` or on ucloud.bupt.edu.cn. Do NOT resubmit "
    "blindly — every submit overwrites the previous one."
)

# Exceptions that make a single read-only check unusable (reported as `error`,
# never as `failed`). CampusError / httpx.HTTPError / OSError / ValueError are
# the expected upstream failures; catching Exception as well is deliberate: a
# crash inside verification must never turn a successful submit into a generic
# exit-5 error (the agent might then resubmit).
_CHECK_ERRORS = Exception

_TEXT_LIMIT = 200


def submit_view_has_submission(submit_view: Any) -> bool:
    """True when submit-view describes a real (not sentinel) submission."""
    if not isinstance(submit_view, dict):
        return False
    if submit_view.get("assignmentId") not in (-1, "-1", None, ""):
        return True
    attachment_ids = submit_view.get("attachmentIds")
    return isinstance(attachment_ids, list) and any(
        str(item).strip() for item in attachment_ids if item is not None
    )


def _normalise_text(value: Any) -> str:
    """Collapse whitespace runs and strip, matching submit's rstrip handling."""
    return " ".join(str(value or "").split())


def _truncate(value: str) -> str:
    return value[:_TEXT_LIMIT]


def _error_text(exc: BaseException, secrets: Optional[list[str]]) -> str:
    """One-line, redacted, bounded error text (same treatment as show)."""
    message = " ".join(str(exc).split()) or exc.__class__.__name__
    return redact(message, secrets or [])[:_TEXT_LIMIT]


def _check_submit_view(
    fetch_submit_view: Callable[[], Any],
    expected_attachment_ids: list[str],
    expected_text: Optional[str],
    secrets: Optional[list[str]],
    compare_attachments: bool = True,
) -> dict[str, Any]:
    expected_ids = [str(item) for item in expected_attachment_ids]
    check: dict[str, Any] = {
        "status": "failed",
        "expected_attachment_ids": expected_ids,
        "observed_attachment_ids": [],
        "has_submission": False,
        "content_checked": False,
    }
    try:
        submit_view = fetch_submit_view()
    except _CHECK_ERRORS as exc:
        check["status"] = "error"
        check["error"] = _error_text(exc, secrets)
        return check

    if not isinstance(submit_view, dict):
        submit_view = {}

    has_submission = submit_view_has_submission(submit_view)
    raw_ids = submit_view.get("attachmentIds")
    observed_ids = [str(item) for item in raw_ids] if isinstance(raw_ids, list) else []
    content_checked = expected_text is not None and bool(str(expected_text).strip())

    expected_normalised = ""
    observed_text = ""
    if content_checked:
        expected_normalised = _normalise_text(expected_text)
        observed_text = _normalise_text(
            html_to_text(str(submit_view.get("assignmentContent") or ""))
        )

    # Compare the attachment set only when the caller knows what to expect.
    # A post-submit check always does (it just uploaded them, even if the set
    # is empty). A bare `bupt homework verify` without --resource-id does not,
    # so it asserts only that a submission is visible; otherwise every existing
    # submission would be reported unconfirmed.
    if compare_attachments:
        attachments_match = set(observed_ids) == set(expected_ids)
    else:
        attachments_match = True
    passed = has_submission and attachments_match
    if content_checked:
        passed = passed and observed_text == expected_normalised

    check.update(
        {
            "status": "passed" if passed else "failed",
            "observed_attachment_ids": observed_ids,
            "has_submission": has_submission,
            "content_checked": content_checked,
            "attachments_checked": bool(compare_attachments),
        }
    )
    if content_checked:
        check["expected_text"] = _truncate(str(expected_text))
        check["observed_text"] = _truncate(observed_text)
    return check


def _check_list(
    fetch_list_record: Callable[[], Any],
    secrets: Optional[list[str]],
) -> dict[str, Any]:
    check: dict[str, Any] = {
        "status": "failed",
        "found": False,
        "submitted": False,
        "submitted_at": None,
    }
    try:
        record = fetch_list_record()
    except _CHECK_ERRORS as exc:
        check["status"] = "error"
        check["error"] = _error_text(exc, secrets)
        return check

    if record is None:
        return check

    submit_time = getattr(record, "submit_time", None)
    submitted = bool(submit_time) or bool(getattr(record, "is_submitted", False))
    check.update(
        {
            "status": "passed" if submitted else "failed",
            "found": True,
            "submitted": submitted,
            "submitted_at": submit_time or None,
        }
    )
    return check


def verify_submission(
    *,
    fetch_submit_view: Callable[[], Any],
    fetch_list_record: Callable[[], Any],
    assignment_id: str,
    expected_attachment_ids: list[str],
    expected_text: Optional[str],
    wait: float,
    sleep: Callable[[float], Any] = time.sleep,
    attempts: int = 2,
    secrets: Optional[list[str]] = None,
    compare_attachments: bool = True,
) -> dict[str, Any]:
    """Read-only verification of a submission.

    The first attempt runs immediately; ``wait`` seconds are slept only before
    a *retry*, since waiting before the first try just delays the answer. The
    upstream
    is eventually consistent, so a failed first attempt is retried once by
    default. The final status is decided from the *last* attempt:

    * any check failed -> ``failed`` (the submit reported success but the data
      is not confirmed);
    * else any check errored -> ``error`` (verification could not run);
    * else -> ``passed``.

    ``assignment_id`` is accepted for call-site clarity/logging; the injected
    callables are already bound to it.
    """
    del assignment_id  # unused; the callables are bound to the assignment
    wait = float(wait)
    checks: dict[str, Any] = {}
    last_attempt = 0
    for attempt in range(1, max(1, int(attempts)) + 1):
        if attempt > 1:
            sleep(wait)
        last_attempt = attempt
        checks = {
            "submit_view": _check_submit_view(
                fetch_submit_view,
                expected_attachment_ids,
                expected_text,
                secrets,
                compare_attachments=compare_attachments,
            ),
            "list": _check_list(fetch_list_record, secrets),
        }
        if all(check.get("status") == "passed" for check in checks.values()):
            break

    failed = [name for name, check in checks.items() if check.get("status") == "failed"]
    errored = [name for name, check in checks.items() if check.get("status") == "error"]
    if failed:
        status = "failed"
    elif errored:
        status = "error"
    else:
        status = "passed"

    result: dict[str, Any] = {
        "status": status,
        "attempts": last_attempt,
        "wait_seconds": wait,
        "checks": checks,
    }
    if status == "failed":
        result["failed_checks"] = failed
        result["hint"] = MANUAL_CHECK_HINT
    elif status == "error":
        result["hint"] = MANUAL_CHECK_HINT
    return result
