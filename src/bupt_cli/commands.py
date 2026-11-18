"""Implementations of the ``bupt`` subcommands."""

from __future__ import annotations

import datetime as dt
import os
import re
import time
from typing import Any, Optional

from bupt_cli._vendor.campus_api.bupt.constants import (
    NOTICE_TYPE_NAME_MAP,
    BuptBuilding,
    CampusArea,
)
from bupt_cli._vendor.campus_api.bupt.models import html_to_text
from bupt_cli.config import load_dorm_config
from bupt_cli.credentials import load_credentials
from bupt_cli.errors import (
    EXIT_SUBMIT_UNVERIFIED,
    EXIT_VERIFY_ERROR,
    AuthError,
    UsageError,
    redact,
)
from bupt_cli.service import (
    assignment_dict,
    course_dict,
    detail_dict,
    download_url_to_path,
    filter_assignments,
    get_submit_view,
    parse_datetime,
    publish_time,
    resolve_attachment_urls,
    status_for,
    submit_homework,
    upload_homework_file,
)
from bupt_cli.session import (
    BuptSession,
    delete_session,
    session_path,
)
from bupt_cli.verify import (
    MANUAL_CHECK_HINT,
    VERIFY_ONLY_HINT,
    submit_view_has_submission as _submit_view_has_submission,
    verify_submission,
)

# --- Module-level constants -------------------------------------------------
# Chinese digits used to parse `学十一楼`-style building names.
_CN_DIGITS: dict[str, int] = {
    "零": 0, "一": 1, "二": 2, "三": 3, "四": 4,
    "五": 5, "六": 6, "七": 7, "八": 8, "九": 9,
}

# Environment variables that override the dorm flags, keyed by field name.
_DORM_ENV: dict[str, str] = {
    "building": "BUPT_DORM_BUILDING",
    "room": "BUPT_DORM_ROOM",
    "floor": "BUPT_DORM_FLOOR",
}

# Built-in placeholder dorm used when nothing else is configured. It is a
# documented example (西土城 学一楼 101), not anyone's real room. No floor is
# given: it is inferred from the room number, so `floor` stays None here.
DEFAULT_DORM: dict[str, str] = {
    "building": "学一楼",
    "room": "101",
}

# Where `bupt update` looks for releases. Override with --repo or BUPT_CLI_REPO.
DEFAULT_REPO = "tututuhehehe/bupt-cli"
# Release asset name produced by build.sh + the release workflow.
ASSET_NAME = "bupt-macos-arm64.zip"
# `homework list --new` resolves publish times through the detail endpoint; cap
# how many it looks up so a long course list cannot stall the command.
PUBLISH_LOOKUP_LIMIT = 30
# Upper bound on course-list pages fetched by `bupt courses`.
COURSE_PAGE_LIMIT = 10
# Page size for the same listing; the terminator compares against it.
COURSE_PAGE_SIZE = 50
# Seconds to wait between read-only post-submit verification attempts.
VERIFY_WAIT_SECONDS = 5.0


# ---------------------------------------------------------------------------
# Small shared helpers.
# ---------------------------------------------------------------------------


def _session(args, *, required: bool = True) -> BuptSession:
    credentials = load_credentials(required=required)
    return BuptSession(credentials, timeout=getattr(args, "timeout", 20.0))


def _client(session: BuptSession, *, allow_mobile_login: bool = False):
    return session.client(allow_mobile_login=allow_mobile_login)


def _persist(session: BuptSession) -> None:
    # A failed cache write must not turn a successful read-only command into
    # an error; `bupt login` persists explicitly (and surfaces write errors).
    try:
        session.persist()
    except OSError:
        pass


def _dedupe(items: list) -> list:
    seen: set[str] = set()
    result = []
    for item in items:
        key = getattr(item, "activity_id", "") or id(item)
        if key in seen:
            continue
        seen.add(key)  # type: ignore[arg-type]
        result.append(item)
    return result


def _resolve_building(value: str) -> BuptBuilding:
    raw = str(value).strip()
    key = raw.upper().replace("-", "_")
    for member in BuptBuilding:
        if member.name == key or member.partment_name == raw:
            return member
    alias = _BUILDING_ALIASES.get(raw)
    if alias is not None:
        return alias
    choices = ", ".join(member.name for member in BuptBuilding)
    raise UsageError(
        f"unknown building '{value}'",
        hint=f"known buildings: {choices}; run `bupt electricity buildings "
        "--area xitucheng` (or --area shahe) for the upstream list",
    )


def _chinese_number(text: str) -> Optional[int]:
    """Parse a small Chinese numeral such as 五, 十, 十一 or 二十九."""
    if not text:
        return None
    if "十" in text:
        left, _, right = text.partition("十")
        tens = _CN_DIGITS.get(left, 1) if left else 1
        ones = _CN_DIGITS.get(right, 0) if right else 0
        if (left and left not in _CN_DIGITS) or (right and right not in _CN_DIGITS):
            return None
        return tens * 10 + ones
    if len(text) == 1:
        return _CN_DIGITS.get(text)
    return None


def _build_building_aliases() -> dict[str, BuptBuilding]:
    """Short building aliases (学五楼 / 学5楼 / 学5 / 5) per enum member."""
    aliases: dict[str, BuptBuilding] = {}
    for member in BuptBuilding:
        aliases.setdefault(member.partment_name, member)
        match = re.search(r"学([零一二三四五六七八九十]+)楼", member.partment_name)
        if not match:
            continue
        number = _chinese_number(match.group(1))
        if number is None:
            continue
        for alias in (f"学{number}楼", f"学{number}", f"{number}楼", str(number)):
            aliases.setdefault(alias, member)
    return aliases


# Built once at import: `_resolve_building` falls back to this table for short
# spellings like 学5 or "5" that are not an enum name or official partment_name.
_BUILDING_ALIASES: dict[str, BuptBuilding] = _build_building_aliases()


def resolve_dorm(args) -> dict[str, Any]:
    """Resolve building/room/floor: flag > env > config.json > default dorm."""
    flags = {
        "building": getattr(args, "building", None),
        "room": getattr(args, "room", None),
        "floor": getattr(args, "floor", None),
    }
    # Spec's "simplest rule": when --building is passed, --room must be passed
    # too. We deliberately do not fall back to the env/config room here, even
    # if it happens to match that building.
    if flags["building"] and not flags["room"]:
        raise UsageError(
            "--room is required when --building is given",
            hint="e.g. `bupt electricity --building 学一楼 --room 101`",
        )

    config = load_dorm_config()
    values: dict[str, Optional[str]] = {}
    sources: dict[str, str] = {}
    for field in ("building", "room", "floor"):
        flag = flags[field]
        env = os.environ.get(_DORM_ENV[field])
        configured = config.get(field)
        if flag:
            values[field], sources[field] = str(flag).strip(), "flag"
        elif env and env.strip():
            values[field], sources[field] = env.strip(), "env"
        elif configured:
            values[field], sources[field] = configured, "config"
        else:
            values[field] = DEFAULT_DORM.get(field)
            sources[field] = "default"

    building_value = values["building"] or DEFAULT_DORM["building"]
    return {
        "building": _resolve_building(building_value),
        "room": values["room"],
        "floor": values["floor"],
        "source": sources["room"],
    }


def _resolve_area(value: str) -> CampusArea:
    mapping = {
        "shahe": CampusArea.SHAHE,
        "沙河": CampusArea.SHAHE,
        "2": CampusArea.SHAHE,
        "xitucheng": CampusArea.XITUCHENG,
        "haidian": CampusArea.XITUCHENG,
        "西土城": CampusArea.XITUCHENG,
        "海淀": CampusArea.XITUCHENG,
        "1": CampusArea.XITUCHENG,
    }
    key = value.strip().lower()
    if key not in mapping:
        raise UsageError("--area must be 'shahe' or 'xitucheng'")
    return mapping[key]


# ---------------------------------------------------------------------------
# Session commands.
# ---------------------------------------------------------------------------


def _prompt_credentials(args) -> tuple[str, str, str]:
    """Collect account details from flags, then stdin for anything missing."""
    import getpass
    import sys

    from bupt_cli.credentials import ROLE_LABELS, credentials_path, normalize_role

    existing = {}
    try:
        existing = load_credentials(required=False)
    except Exception:
        existing = None

    print(f"配置文件: {credentials_path()}", file=sys.stderr)

    username = (args.user or "").strip()
    if not username:
        if existing and existing.username:
            username = existing.username
        else:
            username = input("学号：").strip()

    password = args.password or ""
    if not password and existing and existing.password and existing.username == username:
        password = existing.password
    if not password:
        password = getpass.getpass("密码（输入时不显示）：")

    role = normalize_role(args.role)
    if not role and existing and existing.username == username and existing.role:
        role = existing.role
    if not role:
        while True:
            answer = input(
                "身份（1=研究生 2=本科生）[" + (
                    ROLE_LABELS.get(existing.role, "") if existing and existing.role else ""
                ) + "]: "
            ).strip()
            if answer in ("1", ""):
                role = "graduate"
            elif answer == "2":
                role = "undergraduate"
            else:
                role = normalize_role(answer)
            if role:
                break
            print("请输入 1 或 2", file=sys.stderr)

    if not username or not password:
        raise UsageError("学号和密码都不能为空")
    return username, password, role


def cmd_login(args) -> dict[str, Any]:
    from bupt_cli.credentials import ROLE_LABELS, save_credentials

    interactive = not (args.user and args.password and args.role)
    if interactive:
        username, password, role = _prompt_credentials(args)
    else:
        username, password = args.user.strip(), args.password
        from bupt_cli.credentials import normalize_role

        role = normalize_role(args.role)
        if not role:
            raise UsageError("--role 需要是 graduate 或 undergraduate")

    path = save_credentials(username, password, role)

    credentials = load_credentials()
    session = BuptSession(credentials, timeout=getattr(args, "timeout", 20.0))
    try:
        result = session.login(
            force=args.force, try_mobile=getattr(args, "jwgl", False)
        )
    finally:
        # Keep CAS cookies even when the UCloud token exchange fails.
        try:
            session.persist()
        except OSError:
            pass
    result["credentials_path"] = str(path)
    result["role"] = role
    result["role_label"] = ROLE_LABELS.get(role, role)
    return result


def cmd_status(args) -> dict[str, Any]:
    session = _session(args, required=False)
    status = session.status()
    if getattr(args, "check", False):
        status["live"] = session.live_check()
    return status


def cmd_logout(args) -> dict[str, Any]:
    removed = delete_session()
    return {"removed": removed, "path": str(session_path())}


# ---------------------------------------------------------------------------
# UCloud homework / courses.
# ---------------------------------------------------------------------------


def _enrich_undone_courses(client, items, args) -> None:
    """The undone endpoint returns siteId=-1/siteName="" — map ids to courses."""
    if not any(not item.site_name or item.site_id in (None, "-1") for item in items):
        return
    try:
        all_items = client.get_all_homework(
            site_limit=getattr(args, "max_courses", 30),
            page_size=getattr(args, "page_size", 50),
        )
    except Exception:
        return
    by_id = {item.activity_id: item for item in all_items}
    for item in items:
        match = by_id.get(item.activity_id)
        if not match:
            continue
        if match.site_name:
            object.__setattr__(item, "site_name", match.site_name)
        if match.site_id:
            object.__setattr__(item, "site_id", match.site_id)


def _load_assignments(client, args) -> tuple[list, str]:
    course_id = getattr(args, "course", None)
    if course_id:
        items = client.homework.get_course_homework(
            course_id, page_size=getattr(args, "page_size", 50)
        )
        method = "course"
    elif getattr(args, "undone", False):
        overview = client.get_undone_homework()
        items = overview.undone_list
        method = "undone"
    else:
        items = client.get_all_homework(
            site_limit=getattr(args, "max_courses", 30),
            page_size=getattr(args, "page_size", 50),
        )
        method = "all"
    return _dedupe(list(items)), method


def cmd_homework_list(args) -> dict[str, Any]:
    """List assignments; ``--new N`` narrows to recently published ones."""
    session = _session(args)
    client = _client(session)
    try:
        items, method = _load_assignments(client, args)
        _enrich_undone_courses(client, items, args)

        new_days = getattr(args, "new", None)
        selected = filter_assignments(
            items,
            course_id=args.course,
            undone_only=args.undone,
        )

        published_at: dict[str, dt.datetime] = {}
        unknown: list = []
        stats: dict[str, Any] = {}
        if new_days is not None:
            cutoff = dt.datetime.now() - dt.timedelta(days=new_days)
            published_at, unknown, stats = _resolve_publish_times(client, selected)
            fresh = [
                item
                for item in selected
                if published_at.get(str(item.activity_id)) is not None
                and published_at[str(item.activity_id)] >= cutoff
            ]
            fresh.sort(
                key=lambda item: published_at[str(item.activity_id)],
                reverse=True,
            )
            selected = fresh

        truncated = False
        if args.limit and args.limit > 0 and len(selected) > args.limit:
            truncated = True
            selected = selected[: args.limit]

        assignments = []
        for item in selected:
            entry = assignment_dict(item)
            stamp = published_at.get(str(item.activity_id))
            if stamp is not None:
                entry["published_at"] = stamp.isoformat(sep=" ")
            assignments.append(entry)

        payload: dict[str, Any] = {
            "count": len(assignments),
            "method": method,
            "filters": {
                "undone": bool(args.undone),
                "course": args.course,
                "new_days": new_days,
            },
            "assignments": assignments,
        }
        if new_days is not None:
            payload["publish_time"] = {
                "looked_up": stats.get("looked_up", 0),
                "resolved": len(published_at),
                "unknown": len(unknown),
                "failed": stats.get("failed", 0),
                "lookup_limit": PUBLISH_LOOKUP_LIMIT,
            }
            if unknown:
                # Never drop assignments silently: say how many could not be
                # dated, so a "0 results" answer cannot be misread as "nothing
                # new was assigned".
                payload["unknown_publish_time"] = [str(i.activity_id) for i in unknown]
            if truncated:
                payload["truncated"] = True
            payload["note"] = _new_filter_note(
                items_total=len(items),
                selected_before=len(
                    filter_assignments(
                        items,
                        course_id=args.course,
                        undone_only=args.undone,
                    )
                ),
                stats=stats,
                unknown=len(unknown),
            )
        return payload
    finally:
        _persist(session)


def _new_filter_note(
    *,
    items_total: int,
    selected_before: int,
    stats: dict[str, Any],
    unknown: int,
) -> str:
    """Explain what ``--new`` did, including anything it could not decide."""
    if items_total == 0:
        return "没有作业（上游返回空列表）"
    if selected_before == 0:
        return "过滤条件（--course / --undone）下没有作业"
    resolved = stats.get("resolved", 0)
    looked_up = stats.get("looked_up", 0)
    failed = stats.get("failed", 0)
    parts = [
        f"按发布时间过滤：{selected_before} 条候选中解析出 {resolved} 条发布时间"
    ]
    if looked_up:
        parts.append(f"逐个查详情 {looked_up} 次（上限 {PUBLISH_LOOKUP_LIMIT}）")
    if failed:
        parts.append(f"{failed} 次详情请求失败")
        error = stats.get("last_error")
        if error:
            parts.append(f"最后一个错误：{error}")
    if unknown:
        parts.append(
            f"{unknown} 条拿不到发布时间，已排除（见 unknown_publish_time），"
            "结果可能不完整"
        )
    if resolved and not unknown:
        # Everything was dated; an empty result now genuinely means "nothing
        # recent", which is worth saying so it is not read as a lookup failure.
        parts.append("没有落在时间窗口内的作业")
    return "；".join(parts)


def _looks_like_auth_error(exc: BaseException) -> bool:
    """True for credential/session failures that the user must act on."""
    from bupt_cli._vendor.campus_api import exceptions as _campus_exc

    if isinstance(exc, _campus_exc.CampusAuthError):
        return True
    text = str(exc).lower()
    return any(
        marker in text
        for marker in ("401", "403", "unauthorized", "token expired",
                       "invalid token", "login required", "not logged in")
    )


def _resolve_publish_times(
    client, targets: list
) -> tuple[dict[str, dt.datetime], list, dict[str, Any]]:
    """Recover publish times for the assignments we are about to filter.

    ``targets`` must already be narrowed by --course/--undone: spending the
    lookup budget on assignments the caller filtered out would starve the ones
    that actually matter.

    Ordered newest-deadline-first because publication order tracks deadlines,
    so a truncated budget still finds the recent batches. Everything that could
    not be dated is returned separately instead of being dropped silently.
    """
    ordered = sorted(
        targets,
        key=lambda item: (
            item.parsed_end_time is None,
            item.parsed_end_time or dt.datetime.min,
        ),
        reverse=True,
    )

    published: dict[str, dt.datetime] = {}
    looked_up = 0
    failed = 0
    last_error: Optional[str] = None
    budget = PUBLISH_LOOKUP_LIMIT

    # A list row that already carries a publish time needs no detail call.
    for item in ordered:
        activity_id = str(getattr(item, "activity_id", "") or "")
        if not activity_id:
            continue
        parsed = publish_time(item)
        if parsed is not None:
            published[activity_id] = parsed

    for item in ordered:
        activity_id = str(getattr(item, "activity_id", "") or "")
        if not activity_id or activity_id in published:
            continue
        if budget <= 0:
            break
        looked_up += 1
        try:
            detail = client.get_homework_detail(activity_id)
        except Exception as exc:  # recorded below, never silent
            # A failed lookup must not consume budget, otherwise a transient
            # run of errors starves the assignments that would have worked.
            # Auth failures are re-raised: reporting them as "no publish time"
            # would tell the user to retry instead of to log in again.
            if _looks_like_auth_error(exc):
                raise
            failed += 1
            last_error = _short_error(exc)
            continue
        budget -= 1
        parsed = parse_datetime(
            detail.raw.get("assignmentBeginTime")
            or detail.raw.get("publishTime")
            or detail.raw.get("createTime")
        )
        if parsed is not None:
            published[activity_id] = parsed

    unknown = [
        item
        for item in targets
        if str(getattr(item, "activity_id", "") or "") not in published
    ]
    stats = {
        "looked_up": looked_up,
        "resolved": len(published),
        "failed": failed,
        "last_error": last_error,
    }
    return published, unknown, stats


def _short_error(exc: BaseException, limit: Optional[int] = 200) -> str:
    """Compact one-line error text for best-effort fields."""
    message = " ".join(str(exc).split()) or exc.__class__.__name__
    return message if limit is None else message[:limit]


def _redaction_secrets(session, token: str) -> list[str]:
    secrets = [token] if token else []
    credentials = getattr(session, "credentials", None)
    getter = getattr(credentials, "secret_values", None)
    if callable(getter):
        try:
            secrets.extend(getter())
        except Exception:
            pass
    return secrets


def _submit_view_score(value: Any) -> Optional[float]:
    """Submit-view score only when it is a real grade (>= 0)."""
    if isinstance(value, bool):
        return None
    parsed: Optional[float] = None
    if isinstance(value, (int, float)):
        parsed = float(value)
    elif isinstance(value, str) and value.strip():
        try:
            parsed = float(value)
        except ValueError:
            return None
    if parsed is None or parsed < 0:
        return None
    return parsed


def _submit_view_attachments(
    raw_ids: Any, token: str, timeout: float
) -> list[dict[str, Any]]:
    """Build and resolve the student's submitted attachments."""
    if not isinstance(raw_ids, list):
        return []
    attachments: list[dict[str, Any]] = []
    for resource_id in raw_ids:
        text = str(resource_id).strip() if resource_id is not None else ""
        if not text:
            continue
        attachments.append(
            {
                "title": f"attachment-{len(attachments) + 1}",
                "url": None,
                "resource_id": text,
                "size": None,
            }
        )
    if not attachments:
        return []
    return resolve_attachment_urls(
        attachments, token, timeout=timeout, derive_titles=True
    )


def _target_site_name(detail) -> Optional[str]:
    raw = getattr(detail, "raw", None) or {}
    return (
        raw.get("className")
        or raw.get("siteName")
        or raw.get("site_name")
        or None
    )


def _find_list_record(
    client,
    assignment_id: str,
    *,
    course_id: Optional[str] = None,
    target_name: Optional[str] = None,
):
    """Look up a student-list record with the site-selection used by show.

    This is a read-only helper shared by `homework show`, `homework submit
    --verify` and `homework verify`.
    """
    if course_id:
        site_ids = [str(course_id)]
    else:
        courses = client.get_enrolled_courses()
        site_ids = [
            str(course.site_id)
            for course in courses
            if course.site_id and target_name and course.site_name == target_name
        ]
        if not site_ids:
            site_ids = [str(course.site_id) for course in courses if course.site_id]
    return client.homework.find_list_record(str(assignment_id), site_ids)


def _safe_target_name(client, assignment_id: str) -> Optional[str]:
    """Best-effort course name for site narrowing; never fail verification."""
    try:
        detail = client.get_homework_detail(str(assignment_id))
    except Exception:
        return None
    return _target_site_name(detail)


def _lookup_submitted_at(client, args, detail) -> Optional[str]:
    """Find the submission time in the student list; never fail the command."""
    try:
        record = _find_list_record(
            client,
            args.id,
            course_id=getattr(args, "course", None),
            target_name=_target_site_name(detail),
        )
    except Exception:
        return None
    submit_time = getattr(record, "submit_time", None) if record else None
    return submit_time or None


def _verify_wait(args) -> float:
    """Seconds between read-only verification attempts (fixed, not a flag)."""
    return VERIFY_WAIT_SECONDS


def _verification_payload(verification: dict[str, Any]) -> dict[str, Any]:
    """Shape a verification result for stdout and attach the private exit code."""
    status = verification.get("status")
    list_check = (verification.get("checks") or {}).get("list") or {}
    submitted_at = (
        list_check.get("submitted_at") if status != "skipped" else None
    )
    payload: dict[str, Any] = {
        "verified": None,
        "submitted_at": submitted_at,
        "verification": verification,
    }
    if status == "passed":
        payload["verified"] = True
    elif status == "failed":
        payload["verified"] = False
        payload["_exit_code"] = EXIT_SUBMIT_UNVERIFIED
    elif status == "error":
        summaries = [
            f"{name}: {check['error']}"
            for name, check in (verification.get("checks") or {}).items()
            if check.get("status") == "error" and check.get("error")
        ]
        summary = "; ".join(summaries) or "verification could not run"
        hint = verification.get("hint") or MANUAL_CHECK_HINT
        payload["warning"] = f"verification could not run: {summary}. {hint}"
        payload["_exit_code"] = EXIT_VERIFY_ERROR
    return payload


def _run_verification(
    session,
    token: str,
    *,
    assignment_id: str,
    fetch_submit_view,
    fetch_list_record,
    expected_attachment_ids: list[str],
    expected_text: Optional[str],
    wait: float,
    hint: Optional[str] = None,
    compare_attachments: bool = True,
) -> dict[str, Any]:
    """Run the read-only verification with command-level redaction/sleep."""
    verification = verify_submission(
        fetch_submit_view=fetch_submit_view,
        fetch_list_record=fetch_list_record,
        assignment_id=str(assignment_id),
        expected_attachment_ids=[str(item) for item in expected_attachment_ids],
        expected_text=expected_text,
        wait=wait,
        sleep=time.sleep,
        secrets=_redaction_secrets(session, token),
        compare_attachments=compare_attachments,
    )
    if hint and verification.get("status") in ("failed", "error"):
        verification["hint"] = hint
    return _verification_payload(verification)


def cmd_homework_show(args) -> dict[str, Any]:
    session = _session(args)
    client = _client(session)
    try:
        detail = client.get_homework_detail(args.id)
        payload = detail_dict(detail)
        payload["course"] = (
            detail.raw.get("siteName")
            or detail.raw.get("site_name")
            or detail.raw.get("className")
            or None
        )
        payload["published_at"] = (
            parse_datetime(detail.raw.get("assignmentBeginTime")).isoformat(sep=" ")
            if parse_datetime(detail.raw.get("assignmentBeginTime"))
            else None
        )
        payload["deadline"] = detail.raw.get("assignmentEndTime")
        payload["status"] = status_for(
            bool(detail.is_submitted), detail.raw.get("assignmentEndTime")
        )
        token_data = client._cas_session_mgr.get_ucloud_token()
        token = token_data["access_token"]
        timeout = getattr(args, "timeout", 20.0)
        payload["attachments"] = resolve_attachment_urls(
            payload["attachments"], token, timeout=timeout
        )

        # `/work/detail` never reports the student's own submission; the
        # submit-view endpoint does. It is best effort: a failure must not
        # break `homework show`.
        submitted = bool(detail.is_submitted)
        submit_view: dict[str, Any] = {}
        submit_view_error: Optional[str] = None
        try:
            submit_view = get_submit_view(token, args.id, timeout=timeout)
        except Exception as exc:
            # Redact the full message first: truncating before redaction could
            # leave the prefix of a partially-cut secret in the output.
            submit_view_error = redact(
                _short_error(exc, limit=None), _redaction_secrets(session, token)
            )[:200]
        else:
            if submit_view:
                submitted = _submit_view_has_submission(submit_view)
                if submitted:
                    content = html_to_text(
                        str(submit_view.get("assignmentContent") or "")
                    )
                    payload["submit_content"] = content or None
                    payload["submit_attachments"] = _submit_view_attachments(
                        submit_view.get("attachmentIds"), token, timeout
                    )
                    score = _submit_view_score(submit_view.get("score"))
                    if score is not None:
                        payload["score"] = score
                    evaluation = submit_view.get("evaluation")
                    if isinstance(evaluation, str) and evaluation.strip():
                        payload["teacher_comment"] = evaluation
            else:
                # HTTP-200 error envelopes also decode to {}; keep the
                # detail-derived answer instead of re-introducing the bug.
                submit_view_error = "submit-view returned no data"

        payload["submitted"] = submitted
        if submitted:
            payload["submitted_at"] = _lookup_submitted_at(client, args, detail)
        else:
            payload["submit_content"] = None
            payload["submit_attachments"] = []
            payload["submitted_at"] = None
        if submit_view_error:
            payload["submit_view_error"] = submit_view_error
        return {"assignment": payload}
    finally:
        _persist(session)


def cmd_courses(args) -> dict[str, Any]:
    """Every enrolled course site, fetched across pages.

    A student has a handful of current courses plus older semester sites, so
    pagination is an implementation detail here rather than a user knob.
    """
    session = _session(args)
    client = _client(session)
    try:
        courses = []
        seen: set[str] = set()
        for page in range(1, COURSE_PAGE_LIMIT + 1):
            batch = client.get_enrolled_courses(size=COURSE_PAGE_SIZE, page=page)
            if not batch:
                break
            fresh = []
            for course in batch:
                site_id = str(getattr(course, "site_id", "") or "")
                # Empty ids cannot be deduplicated without losing courses.
                if site_id and site_id in seen:
                    continue
                if site_id:
                    seen.add(site_id)
                fresh.append(course)
            courses.extend(fresh)
            if not fresh or len(batch) < COURSE_PAGE_SIZE:
                break
        return {
            "count": len(courses),
            "courses": [course_dict(course) for course in courses],
        }
    finally:
        _persist(session)


# ---------------------------------------------------------------------------
# Mobile JWGL commands.
# ---------------------------------------------------------------------------


def _schedule_course(course, dates: list[str]) -> dict[str, Any]:
    date = dates[course.day] if 0 <= course.day < len(dates) else ""
    return {
        "name": course.name,
        "day": course.day,
        "day_name": course.day_name,
        "date": date,
        "sections": course.sections_display,
        "start_section": course.section_start,
        "section_span": course.section_span,
        "time": course.time_str,
        "weeks": course.weeks_display,
        "location": course.location,
        "campus": course.campus,
        "credit": course.credit,
        "attribute": course.attr,
        "classes": course.classes,
    }


def _require_undergraduate(command: str) -> None:
    """Refuse the undergraduate-JWGL commands early for graduate accounts.

    Otherwise the command spends a real login attempt only to surface
    "invalid username or password", which reads like a wrong password and
    invites retries that can lock the account. The registrar system has no
    record of a graduate id at all.
    """
    credentials = load_credentials(required=False)
    if credentials.role == "graduate":
        raise UsageError(
            f"`bupt {command}` 走的是本科教务系统，研究生学号在那套系统里不存在",
            hint="改用 `bupt schedule` / `bupt schedule export` / `bupt schedule exams`；"
            "若身份登记有误，重新执行 `bupt login --role undergraduate`",
        )


def _refresh_timetable(args) -> dict[str, Any]:
    """Fetch the timetable from whichever system the account belongs to."""
    from bupt_cli import timetable

    credentials = load_credentials()
    role = credentials.role

    if role == "undergraduate":
        session = _session(args)
        client = _client(session)
        try:
            entry = timetable.fetch_undergraduate(client)
        finally:
            _persist(session)
        timetable.save(entry)
        return entry

    # Graduate (and unknown roles: try the verified path first).
    try:
        entry = timetable.fetch_graduate(
            credentials.username, credentials.password
        )
    except timetable.TimetableError:
        raise
    except Exception as exc:
        hint = "检查 ~/.bupt-cli/credentials.json 的账号与 role；研究生系统需要 Chrome"
        if role == "":
            hint = (
                "账号身份未登记；`bupt login --role graduate` 或 "
                "`--role undergraduate` 后重试"
            )
        raise AuthError(f"获取课表失败：{exc}", hint=hint) from None
    timetable.save(entry)
    return entry


def _timetable_entry(args) -> tuple[dict[str, Any], bool]:
    """Cached timetable, refreshing only when needed.

    Returns ``(entry, from_cache)``. The whole point of the cache is that
    "what do I have today" costs a file read instead of a browser launch.
    """
    from bupt_cli import timetable

    cached = timetable.load()
    if cached is not None and not timetable.is_stale(cached):
        return cached, True
    return _refresh_timetable(args), False


def cmd_schedule(args) -> dict[str, Any]:
    """Timetable queries, served from the local cache when possible."""
    from bupt_cli import timetable

    sub = getattr(args, "schedule_command", None)
    if sub == "update":
        entry = _refresh_timetable(args)
        bounds = timetable.week_bounds(entry)
        return {
            "ok": True,
            "updated": True,
            "role": entry.get("role"),
            "source": entry.get("source"),
            "term": entry.get("term"),
            "courses": len(entry.get("courses") or []),
            "weeks": len(entry.get("calendar") or {}),
            "range": [bounds[0].isoformat(), bounds[1].isoformat()] if bounds else None,
            "path": str(timetable.cache_path()),
        }
    if sub == "export":
        return cmd_schedule_export(args)
    if sub == "calendar":
        return cmd_schedule_calendar(args)
    if sub == "exams":
        return cmd_schedule_exams(args)

    entry, from_cache = _timetable_entry(args)
    today = dt.date.today()

    # Which dates?
    if getattr(args, "raw", False):
        return {
            "term": entry.get("term"),
            "role": entry.get("role"),
            "from_cache": from_cache,
            "count": len(entry.get("courses") or []),
            "courses": entry.get("courses"),
        }

    if getattr(args, "date", None):
        try:
            day = dt.date.fromisoformat(args.date)
        except ValueError:
            raise UsageError(f"invalid --date '{args.date}' (use YYYY-MM-DD)") from None
        start = end = day
        label = day.isoformat()
    elif getattr(args, "today", False):
        start = end = today
        label = today.isoformat()
    elif getattr(args, "tomorrow", False):
        start = end = today + dt.timedelta(days=1)
        label = start.isoformat()
    elif getattr(args, "week", None):
        key = str(args.week)
        week_dates = (entry.get("calendar") or {}).get(key) or []
        if not week_dates:
            raise UsageError(
                f"第 {args.week} 周不在缓存的学期范围内",
                hint="用 `bupt schedule update` 刷新，或 `bupt schedule --raw` 查看课程周次",
            )
        dates = [d for d in week_dates if d]
        start = dt.date.fromisoformat(dates[0])
        end = dt.date.fromisoformat(dates[-1])
        label = f"第 {args.week} 周"
    else:
        start, end = timetable.day_range(entry, today)
        label = "本周"

    sessions = timetable.occurrences(entry, start, end)
    return {
        "term": entry.get("term"),
        "role": entry.get("role"),
        "from_cache": from_cache,
        "range": [start.isoformat(), end.isoformat()],
        "week": timetable.week_of(entry, start),
        "label": label,
        "count": len(sessions),
        "classes": sessions,
    }


def cmd_schedule_calendar(args) -> dict[str, Any]:
    """The teaching-week -> date table backing the cache."""
    from bupt_cli import timetable

    entry, from_cache = _timetable_entry(args)
    calendar = entry.get("calendar") or {}
    return {
        "term": entry.get("term"),
        "role": entry.get("role"),
        "from_cache": from_cache,
        "week_count": len(calendar),
        "weeks": [
            {
                "week": int(week),
                "monday": dates[0] if dates else "",
                "sunday": dates[-1] if dates else "",
                "dates": dates,
            }
            for week, dates in sorted(
                calendar.items(), key=lambda kv: int(kv[0])
            )
            if isinstance(dates, list)
        ],
    }


def cmd_schedule_export(args) -> dict[str, Any]:
    """Write the cached timetable to an .ics file."""
    from pathlib import Path

    from bupt_cli import timetable

    entry, from_cache = _timetable_entry(args)
    try:
        content = timetable.build_ics(entry)
    except timetable.TimetableError as exc:
        raise UsageError(str(exc), hint=getattr(exc, "hint", "")) from None

    if getattr(args, "out", None):
        out = Path(args.out).expanduser()
    else:
        safe = re.sub(r"[^\w\u4e00-\u9fa5-]+", "-", str(entry.get("term") or "课表")).strip("-")
        out = Path.home() / "Downloads" / f"bupt-{safe}.ics"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(content, encoding="utf-8")

    if not getattr(args, "no_open_folder", False):
        import subprocess as _sp

        _sp.run(
            ["open", "-R", str(out)],
            check=False,
            stdout=_sp.DEVNULL,
            stderr=_sp.DEVNULL,
        )

    return {
        "ok": True,
        "term": entry.get("term"),
        "from_cache": from_cache,
        "courses": len(entry.get("courses") or []),
        "weeks": len(entry.get("calendar") or {}),
        "events": content.count("BEGIN:VEVENT"),
        "path": str(out),
        "hint": "在日历 App 里双击导入；重复导入会生成重复事件",
    }


def cmd_schedule_exams(args) -> dict[str, Any]:
    """Exam arrangements come from the graduate system only."""
    from bupt_cli import grad

    credentials = load_credentials(required=False)
    if credentials.role == "undergraduate":
        raise UsageError(
            "`bupt schedule exams` 目前只读研究生系统的考试安排",
            hint="本科生的考试安排请查看教务处或信息门户",
        )
    pages = _grad_pages(args, ("exams",))
    return grad.parse_exams(pages.get("exams", ""))


def cmd_scores(args) -> dict[str, Any]:
    _require_undergraduate("scores")
    session = _session(args)
    client = _client(session)
    try:
        report = client.get_score_report()
    finally:
        _persist(session)
    courses = report.courses
    if args.semester:
        courses = [c for c in courses if c.semester_name == args.semester]
    semesters: dict[str, list[dict[str, Any]]] = {}
    for course in courses:
        semesters.setdefault(course.semester_name or "unknown", []).append(
            {
                "name": course.course_name,
                "code": course.course_code,
                "credit": course.credit,
                "score": course.fraction,
                "attributes": course.curriculum_attributes,
                "nature": course.course_nature,
                "exam_nature": course.examination_nature,
                "score_flag": course.score_flag,
                "id": course.score_id,
            }
        )
    return {
        "student_id": report.student_id,
        "average_score": report.average_score,
        "semesters": semesters,
        "gpa_entries": report.term_gpas,
        "count": len(courses),
    }


def cmd_exams(args) -> dict[str, Any]:
    _require_undergraduate("exams")
    session = _session(args)
    try:
        service = session.mobile_service(allow_login=False)
        weeks = service.get_exam_weeks()
        schedule = service.get_exam_schedule()
        course_schedule = service.get_exam_course_schedule()
        return {
            "weeks": [{"key": w.key, "value": w.value} for w in weeks],
            "exams": schedule,
            "course_arrangements": course_schedule,
            "note": "mobile JWGL exam endpoints return empty data between semesters",
        }
    finally:
        _persist(session)


def cmd_classrooms(args) -> dict[str, Any]:
    _require_undergraduate("classrooms")
    campus = args.campus
    if campus not in {"01", "04"}:
        raise UsageError("--campus must be 01 (西土城) or 04 (沙河)")
    session = _session(args)
    try:
        service = session.mobile_service(allow_login=False)
        entries = service.get_today_classrooms(campus_id=campus)
        return {
            "campus": campus,
            "count": len(entries),
            "periods": [
                {
                    "raw": entry.classrooms,
                    "rooms": [
                        {"name": name, "capacity": capacity}
                        for name, capacity in entry.parse_classrooms()
                    ],
                }
                for entry in entries
            ],
        }
    finally:
        _persist(session)


# ---------------------------------------------------------------------------
# Portal commands.
# ---------------------------------------------------------------------------


def _notice_types(value: Optional[str]) -> Optional[list[str]]:
    if not value:
        return None
    resolved: list[str] = []
    for raw in value.split(","):
        key = raw.strip().lower()
        if not key:
            continue
        if key not in NOTICE_TYPE_NAME_MAP:
            raise UsageError(
                f"unknown notice type '{key}'",
                hint="choose from notice, news, rules, ways",
            )
        resolved.append(key)
    return resolved or None


def cmd_notices_list(args) -> dict[str, Any]:
    types = _notice_types(args.type)
    session = _session(args)
    client = _client(session)
    try:
        notices = client.get_notices(count=args.count, types=types)
        return {
            "count": len(notices),
            "types": types,
            "notices": [
                {
                    "title": notice.title,
                    "url": notice.url,
                    "author": notice.author,
                    "time": notice.time,
                    "type": notice.type,
                    "type_name": notice.type_name,
                }
                for notice in notices
            ],
        }
    finally:
        _persist(session)


def cmd_notices_show(args) -> dict[str, Any]:
    session = _session(args)
    client = _client(session)
    try:
        detail = client.get_notice_detail(args.url)
        content = detail.content_text
        truncated = False
        if args.max_chars and len(content) > args.max_chars:
            content = content[: args.max_chars]
            truncated = True
        return {
            "notice": {
                "title": detail.title,
                "url": detail.url,
                "author": detail.author,
                "time": detail.time,
                "content_text": content,
                "content_truncated": truncated,
                "attachments": detail.attachments,
                "images": detail.images,
            }
        }
    finally:
        _persist(session)


def cmd_card(args) -> dict[str, Any]:
    session = _session(args)
    client = _client(session)
    try:
        balance = client.get_campus_card_balance()
        return {
            "student_id": balance.student_id,
            "main_wallet": balance.main_wallet,
            "subsidy": balance.subsidy,
            "unread_mail": balance.unread_mail,
        }
    finally:
        _persist(session)


# ---------------------------------------------------------------------------
# Electricity.
# ---------------------------------------------------------------------------


def cmd_electricity(args) -> dict[str, Any]:
    resolved = resolve_dorm(args)
    session = _session(args)
    client = _client(session)
    try:
        info = client.get_electricity(
            building=resolved["building"],
            room=resolved["room"],
            floor=resolved["floor"],
        )
        return {
            "building": info.building,
            "floor": info.floor,
            "room": info.room,
            "surplus": info.surplus,
            "price": info.price,
            "remaining_kwh": info.remaining_kwh,
            "total_consumed_kwh": info.total_consumed_kwh,
            "meter_time": info.time,
            "low_balance": info.is_low_balance(args.threshold),
            "threshold": args.threshold,
            "source": resolved["source"],
        }
    finally:
        _persist(session)


def cmd_electricity_buildings(args) -> dict[str, Any]:
    area = _resolve_area(args.area)
    session = _session(args)
    client = _client(session)
    try:
        buildings = client.get_electricity_buildings(area=area)
        return {
            "area": area.name.lower(),
            "count": len(buildings),
            "buildings": buildings,
        }
    finally:
        _persist(session)


# ---------------------------------------------------------------------------
# Homework download / submit (write paths gated by flags).
# ---------------------------------------------------------------------------


def cmd_homework_download(args) -> dict[str, Any]:
    """Download teacher attachments for one assignment into --dir."""
    from pathlib import Path

    out_dir = Path(args.dir).expanduser().resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    session = _session(args)
    client = _client(session)
    try:
        detail = client.get_homework_detail(args.id)
        payload = detail_dict(detail)
        token_data = client._cas_session_mgr.get_ucloud_token()
        token = token_data["access_token"]
        attachments = resolve_attachment_urls(
            payload["attachments"],
            token,
            timeout=getattr(args, "timeout", 20.0),
        )
        saved = []
        skipped = []
        for index, att in enumerate(attachments):
            url = att.get("url")
            title = att.get("title") or f"attachment-{index + 1}"
            # sanitize filename
            safe = "".join(ch if ch not in '\\/:*?"<>|' else "_" for ch in str(title))
            dest = out_dir / safe
            if dest.exists() and not getattr(args, "force", False):
                skipped.append({"title": title, "path": str(dest), "reason": "exists"})
                continue
            if not url:
                skipped.append({"title": title, "path": None, "reason": "no_url"})
                continue
            meta = download_url_to_path(
                url,
                dest,
                timeout=getattr(args, "timeout", 60.0),
                token=token,
            )
            saved.append(
                {
                    "title": title,
                    "resource_id": att.get("resource_id"),
                    **meta,
                }
            )
        return {
            "assignment_id": args.id,
            "title": detail.title,
            "dir": str(out_dir),
            "saved": saved,
            "skipped": skipped,
            "count_saved": len(saved),
            "count_skipped": len(skipped),
        }
    finally:
        _persist(session)


def cmd_homework_submit(args) -> dict[str, Any]:
    """Upload files and/or text, then optionally submit.

    Safety:
    * ``--dry-run`` never uploads and never submits; it only validates inputs
      and shows the payload that would be sent.
    * Real submit requires ``--i-confirm-submit``.
    """
    from pathlib import Path

    files = list(getattr(args, "file", None) or [])
    resource_ids = [str(r) for r in (getattr(args, "resource_id", None) or [])]
    text = getattr(args, "text", None) or ""
    dry_run = bool(getattr(args, "dry_run", False))
    confirm = bool(getattr(args, "i_confirm_submit", False))
    no_verify = bool(getattr(args, "no_verify", False))
    verify_wait = _verify_wait(args)

    if dry_run and confirm:
        raise UsageError("--dry-run cannot be combined with --i-confirm-submit")
    if not files and not resource_ids and not text.strip():
        raise UsageError(
            "nothing to submit",
            hint="pass --file PATH and/or --text CONTENT (or --resource-id)",
        )

    paths: list[Path] = []
    for raw in files:
        path = Path(raw).expanduser().resolve()
        if not path.is_file():
            raise UsageError(f"file not found: {raw}")
        paths.append(path)

    session = _session(args)
    client = _client(session)
    try:
        detail = client.get_homework_detail(args.id)
        token_data = client._cas_session_mgr.get_ucloud_token()
        token = token_data["access_token"]
        user_id = token_data.get("user_id")
        if not user_id:
            raise UsageError("UCloud token missing user_id; run `bupt login --force`")

        submit_view = get_submit_view(
            token, args.id, timeout=getattr(args, "timeout", 20.0)
        )
        # Attachments already attached upstream. Without this, a dry run on an
        # already-submitted assignment reported an empty list and callers could
        # not see what a real submit would replace.
        existing_ids = [
            str(item)
            for item in (submit_view.get("attachmentIds") or [])
            if str(item).strip()
        ]
        planned = {
            "assignment_id": args.id,
            "title": detail.title,
            "already_submitted": bool(detail.is_submitted),
            "text_chars": len(text),
            "files": [
                {"path": str(p), "size": p.stat().st_size, "name": p.name}
                for p in paths
            ],
            "existing_resource_ids": existing_ids,
            "submit_view_status": submit_view.get("status"),
            "submit_view_assignment_status": submit_view.get("assignmentStatus"),
        }

        if dry_run:
            return {
                "dry_run": True,
                "would_upload": [f["name"] for f in planned["files"]],
                "would_submit": True,
                "planned": planned,
                "note": "no upload and no submit performed",
            }

        # Gate before uploading: uploading and then refusing would leave
        # orphaned resources upstream for no reason.
        if not confirm:
            raise UsageError(
                "refusing to submit without --i-confirm-submit",
                hint="先运行 `bupt homework submit ID --dry-run` 核对将要提交的内容，"
                "确认无误后加 --i-confirm-submit",
            )

        uploaded = []
        for path in paths:
            meta = upload_homework_file(
                token,
                str(user_id),
                path,
                timeout=getattr(args, "timeout", 60.0),
            )
            uploaded.append(meta)
            resource_ids.append(meta["resource_id"])

        # de-dupe while preserving order
        seen: set[str] = set()
        attachment_ids: list[str] = []
        for rid in resource_ids:
            if rid and rid not in seen:
                seen.add(rid)
                attachment_ids.append(rid)

        result = submit_homework(
            token,
            str(user_id),
            args.id,
            content=text,
            attachment_ids=attachment_ids,
            timeout=getattr(args, "timeout", 30.0),
        )
        response: dict[str, Any] = {
            "submitted": True,
            "assignment_id": args.id,
            "title": detail.title,
            "uploaded": uploaded,
            "attachment_ids": attachment_ids,
            "text_chars": len(text),
            "upstream": {
                "success": result.get("success"),
                "code": result.get("code"),
                "msg": result.get("msg"),
            },
        }
        if no_verify:
            response.update(_verification_payload({"status": "skipped"}))
            return response

        timeout = getattr(args, "timeout", 20.0)
        target_name = _target_site_name(detail)
        response.update(
            _run_verification(
                session,
                token,
                assignment_id=args.id,
                fetch_submit_view=lambda: get_submit_view(
                    token, args.id, timeout=timeout
                ),
                fetch_list_record=lambda: _find_list_record(
                    client,
                    args.id,
                    course_id=getattr(args, "course", None),
                    target_name=target_name,
                ),
                expected_attachment_ids=attachment_ids,
                expected_text=text or None,
                wait=verify_wait,
            )
        )
        return response
    finally:
        _persist(session)


def cmd_homework_verify(args) -> dict[str, Any]:
    """Read-only check that a submission is visible upstream (never submits)."""
    wait = _verify_wait(args)
    session = _session(args)
    client = _client(session)
    try:
        token_data = client._cas_session_mgr.get_ucloud_token()
        token = token_data["access_token"]
        timeout = getattr(args, "timeout", 20.0)
        expected_ids = [
            str(item) for item in (getattr(args, "resource_id", None) or [])
        ]
        # With no --resource-id there is nothing to compare the upstream
        # attachment list against, so a bare `verify` only asserts that a
        # submission is visible. Pass --resource-id to compare exactly.
        compare_attachments = bool(expected_ids)
        expected_text = getattr(args, "text", None)
        course_id = getattr(args, "course", None)
        target_name = None if course_id else _safe_target_name(client, args.id)
        return _run_verification(
            session,
            token,
            assignment_id=args.id,
            fetch_submit_view=lambda: get_submit_view(
                token, args.id, timeout=timeout
            ),
            fetch_list_record=lambda: _find_list_record(
                client,
                args.id,
                course_id=course_id,
                target_name=target_name,
            ),
            expected_attachment_ids=expected_ids,
            expected_text=expected_text,
            wait=wait,
            hint=VERIFY_ONLY_HINT,
            compare_attachments=compare_attachments,
        )
    finally:
        _persist(session)


# ---------------------------------------------------------------------------
# Library OPAC (anonymous; no session needed).
# ---------------------------------------------------------------------------


def cmd_library_search(args) -> dict[str, Any]:
    """Search the library catalogue by title."""
    from bupt_cli import library

    try:
        payload = library.search(
            args.keyword,
            page=args.page,
            page_size=args.page_size,
            timeout=getattr(args, "timeout", 20.0),
            location=args.location,
        )
        if getattr(args, "with_ebook", False):
            timeout = getattr(args, "timeout", 20.0)
            for book in payload["books"]:
                try:
                    book["has_electronic"] = library.electronic_resources(
                        book["id"], timeout=timeout
                    )["has_electronic"]
                except Exception:
                    # A failed side lookup must not sink the whole search.
                    book["has_electronic"] = None
        return payload
    except library.LibraryError as exc:
        raise UsageError(str(exc)) from None
    except Exception as exc:  # network / JSON shape
        raise UsageError(
            f"library search failed: {exc}",
            hint="opac.bupt.edu.cn must be reachable; try `bupt probe`",
        ) from None


def cmd_library_hold(args) -> dict[str, Any]:
    """Read per-copy holdings for one catalogue record."""
    from bupt_cli import library

    try:
        return library.holdings(args.id, timeout=getattr(args, "timeout", 20.0))
    except library.LibraryError as exc:
        raise UsageError(str(exc)) from None
    except Exception as exc:
        raise UsageError(
            f"library hold failed: {exc}",
            hint="check the id from `bupt library search`; the page is "
            f"{library.OPAC_BASE}/bookInfo_<id>.html",
        ) from None


def cmd_library_ebook(args) -> dict[str, Any]:
    """Electronic resources (online full text) for one catalogue record."""
    from bupt_cli import library

    try:
        return library.electronic_resources(
            args.id, timeout=getattr(args, "timeout", 20.0)
        )
    except library.LibraryError as exc:
        raise UsageError(str(exc)) from None
    except Exception as exc:
        raise UsageError(
            f"library ebook failed: {exc}",
            hint="check the id from `bupt library search`; try `bupt probe` "
            "if opac.bupt.edu.cn is unreachable",
        ) from None


# ---------------------------------------------------------------------------
# Graduate system (yjxt.bupt.edu.cn) — same CAS session, no extra password.
# ---------------------------------------------------------------------------


def _grad_pages(args, want: tuple[str, ...]) -> dict[str, Any]:
    """Fetch the requested graduate pages through a real browser.

    ``args`` carries nothing the collector needs (credentials come from the
    saved account, not the command line), but every caller passes it for a
    uniform call shape.
    """
    from bupt_cli import grad_browser

    credentials = load_credentials()
    if credentials.role == "undergraduate":
        raise UsageError(
            "当前账号登记为本科生，`bupt grad` 只适用于研究生系统",
            hint="改用 `bupt schedule` / `bupt exams`；若身份登记有误，"
            "重新执行 `bupt login --role graduate`",
        )
    try:
        return grad_browser.collect(
            credentials.username, credentials.password, want=want
        )
    except grad_browser.BrowserError as exc:
        raise AuthError(str(exc), hint=exc.hint) from None


def _calendar_year(grad_module, term: str) -> int:
    """Academic year for the calendar, or a clear error when it is unknown.

    Guessing here would silently date every event a year off, which is worse
    than telling the user the page layout changed.
    """
    year = grad_module.academic_year_start(term)
    if year is None:
        raise grad_module.GradError(
            "教学日历页里找不到学期标签，无法确定学年",
            hint="页面结构可能已变；请运行 `bupt grad calendar` 查看原始返回",
        )
    return year


def _versions_tuple(text: str) -> tuple[int, ...]:
    parts = re.findall(r"\d+", text or "")
    return tuple(int(p) for p in parts[:3]) or (0,)


def _release_tag(repo: str, *, timeout: float = 20.0) -> Optional[str]:
    """Latest release tag, read from the `/releases/latest` redirect.

    Deliberately avoids api.github.com: anonymous API access is limited to 60
    requests/hour per IP, which is easy to exhaust and would otherwise force
    users to configure a GITHUB_TOKEN just to check for updates. The web
    redirect has no such limit and needs no credentials.

    Returns "" when the redirect works but names no tag, or None when the repo
    has no releases (or does not exist — GitHub 404s both cases here).
    """
    import urllib.error as _urlerr
    import urllib.request as _urlreq

    class _NoRedirect(_urlreq.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):  # noqa: D102 - verbatim
            return None

    opener = _urlreq.build_opener(_NoRedirect)
    request = _urlreq.Request(
        f"https://github.com/{repo}/releases/latest",
        method="HEAD",
        headers={"User-Agent": "bupt-cli"},
    )
    try:
        with opener.open(request, timeout=timeout) as resp:
            location = resp.headers.get("Location") or resp.url or ""
    except _urlerr.HTTPError as exc:
        location = exc.headers.get("Location") if exc.headers else None
        if not location:
            if exc.code == 404:
                return None
            raise
    tag = str(location).rstrip("/").rsplit("/", 1)[-1]
    return tag if tag and tag != "latest" else ""


def cmd_update(args) -> dict[str, Any]:
    """Compare against the latest GitHub release and replace the binary.

    Uses the public release redirect plus the asset download URL, so no API
    token is ever required.
    """
    import os as _os
    import shutil as _shutil
    import sys as _sys
    import tempfile as _tempfile
    import urllib.request as _urlreq
    import zipfile as _zipfile
    from pathlib import Path

    from bupt_cli import __version__

    repo = args.repo or _os.environ.get("BUPT_CLI_REPO") or DEFAULT_REPO
    result: dict[str, Any] = {"ok": True, "current": __version__, "repo": repo}

    try:
        tag = _release_tag(repo)
    except Exception as exc:
        result.update(
            latest=None,
            update_available=None,
            note=f"检查更新失败：{exc}",
        )
        return result

    if tag is None:
        result.update(
            latest=None,
            update_available=False,
            note=f"{repo} 没有可用的 release（或仓库不存在）；用 --repo 指定仓库",
        )
        return result
    if tag == "":
        result.update(latest=None, update_available=None, note="release 跳转里没有版本号")
        return result

    latest = tag.lstrip("v")
    newer = _versions_tuple(latest) > _versions_tuple(__version__)
    result.update(latest=latest, update_available=newer)
    if not newer:
        result["note"] = "已经是最新版本"
        return result
    if getattr(args, "check", False):
        result["note"] = "有新版本；去掉 --check 即可安装"
        return result

    asset_url = (
        f"https://github.com/{repo}/releases/download/{tag}/{ASSET_NAME}"
    )
    with _urlreq.urlopen(
        _urlreq.Request(asset_url, headers={"User-Agent": "bupt-cli"}), timeout=120
    ) as resp:
        payload = resp.read()

    frozen = bool(getattr(_sys, "frozen", False))
    target = Path(_sys.executable).resolve() if frozen else None
    if target is None:
        result.update(
            installed_to=None,
            note=(
                f"找到新版本 {latest}，但当前不是独立二进制，未自动安装；"
                "请下载 release 附件或运行 ./build.sh 后替换"
            ),
        )
        return result

    with _tempfile.TemporaryDirectory() as tmp:
        archive = Path(tmp) / "bupt.zip"
        archive.write_bytes(payload)
        with _zipfile.ZipFile(archive) as zf:
            member = next(
                (n for n in zf.namelist() if n == "bupt" or n.endswith("/bupt")),
                None,
            )
            if member is None:
                raise UsageError("下载的压缩包里没有 bupt 可执行文件")
            extracted = Path(tmp) / "bupt"
            extracted.write_bytes(zf.read(member))
        extracted.chmod(0o755)

        backup = target.with_suffix(".old")
        _shutil.copy2(target, backup)
        incoming = target.with_suffix(".new")
        _shutil.copy2(extracted, incoming)
        _os.replace(incoming, target)

        # Confirm the replacement actually took effect: reporting "updated"
        # while an older binary survives is worse than failing loudly.
        if not target.is_file() or target.stat().st_size != extracted.stat().st_size:
            raise UsageError(
                "替换二进制失败",
                hint=f"旧版本已备份为 {backup}",
            )

    result["installed_to"] = str(target)
    result["backup"] = str(backup)
    result["note"] = f"已更新到 {latest}，重新运行即可生效"
    return result


def cmd_uninstall(args) -> dict[str, Any]:
    """Remove bupt binaries on PATH and ~/.bupt-cli."""
    import os as _os
    import shutil as _shutil
    import sys as _sys
    from pathlib import Path

    dry_run = bool(getattr(args, "dry_run", False))

    removed: list[str] = []
    skipped: list[str] = []

    # 1. Find every plausible bupt binary.
    #
    #    Searching only PATH is not enough: a user who never installed globally
    #    still has the project launcher on PATH (`bupt-cli/bupt`), and a build
    #    may sit at `dist/bupt` or wherever the binary was unpacked. So collect
    #    candidates from PATH, from the directories around the running program,
    #    and from the usual install locations — then verify each one really is
    #    bupt before deleting it.
    here = Path(__file__).resolve()
    project_root = here.parent.parent.parent  # src/bupt_cli/commands.py -> project

    candidates: list[str] = []

    def add(path: str) -> None:
        if path and path not in candidates:
            candidates.append(path)

    for entry in _os.environ.get("PATH", "").split(_os.pathsep):
        if entry:
            add(_os.path.join(entry, "bupt"))

    # Around the running program (project launcher, the installed ./bupt, an
    # older ./dist/bupt, ...).
    add(str(Path.cwd() / "bupt"))
    add(str(Path.cwd() / "dist" / "bupt"))
    add(str(project_root / "bupt"))
    add(str(project_root / "dist" / "bupt"))
    if _sys.argv and _sys.argv[0]:
        run_path = Path(_sys.argv[0]).resolve()
        add(str(run_path))
        add(str(run_path.parent / "bupt"))
        add(str(run_path.parent / "dist" / "bupt"))

    # Standard install locations.
    for common in ("~/.local/bin/bupt", "/usr/local/bin/bupt", "/opt/homebrew/bin/bupt"):
        add(str(Path(common).expanduser()))

    # The program currently executing: deleting it is legal but leaves the user
    # unable to run `bupt` again, so it is reported instead of removed.
    running_paths: set[str] = set()
    if getattr(_sys, "frozen", False):
        running_paths.add(_os.path.realpath(_sys.executable))
    else:
        # `./bupt-dev` via the launcher: the launcher is $0 of the shell
        # script, which Python does not see. Detect it as the project launcher
        # whose parent holds src/bupt_cli. (`./bupt` is the built binary, not a
        # launcher, so it must stay deletable when running from source.)
        for launcher_name in ("bupt-dev", "bupt"):
            launcher = project_root / launcher_name
            if not launcher.exists():
                continue
            try:
                with open(launcher, "rb") as handle:
                    is_script = handle.read(2) == b"#!"
            except OSError:
                is_script = False
            if is_script:
                running_paths.add(_os.path.realpath(launcher))

    def looks_like_bupt(path: str) -> bool:
        """Strictly verify an executable before deleting it.

        Accepts either our shell launcher (a script mentioning bupt) or a
        PyInstaller binary carrying the bupt_cli archive. A *source* file such
        as ``__main__.py`` mentions bupt_cli but is not executable, so the
        executable bit is checked by the caller.

        PyInstaller appends its archive — including the module names — at the
        *end* of the file, behind a ``MEI`` cookie. Reading only the first 8 KB
        therefore never recognises a real binary, so the tail is inspected too.
        """
        try:
            size = _os.path.getsize(path)
            with open(path, "rb") as handle:
                head = handle.read(8192)
                if size > 65536:
                    handle.seek(-65536, _os.SEEK_END)
                    tail = handle.read()
                else:
                    handle.seek(0)
                    tail = handle.read()
        except OSError:
            return False
        if not head:
            return False

        # Shell launcher: starts with a shebang and names the package.
        if head.startswith(b"#!") or head.startswith(b"#!/bin/sh"):
            text = head.decode("utf-8", "ignore")
            return "bupt" in text and "bupt_cli" in text

        # PyInstaller (Mach-O or ELF). The TOC stores module paths, so require
        # the exact package name — a different tool that merely happens to be
        # called "bupt" would contain b"bupt" but not b"bupt_cli".
        pyinstaller_cookie = b"MEI\x0c\x0b\x0a\x0b\x0e"
        is_macho = head[:4] in (
            b"\xcf\xfa\xed\xfe",
            b"\xfe\xed\xfa\xcf",
            b"\xca\xfe\xba\xbe",
        )
        is_elf = head[:4] == b"\x7fELF"
        if is_macho or is_elf:
            if pyinstaller_cookie in tail:
                return b"bupt_cli" in tail
            # A plain (non-frozen) binary still has to carry our package name.
            return b"bupt_cli" in head or b"bupt_cli" in tail
        return False

    seen: set[str] = set()
    for candidate in candidates:
        if not _os.path.isfile(candidate):
            continue
        # Only executables count: a .py source file can contain the string
        # "bupt_cli" and would otherwise be deleted.
        if not _os.access(candidate, _os.X_OK):
            continue
        real = _os.path.realpath(candidate)
        if real in seen:
            continue
        seen.add(real)

        if real in running_paths:
            skipped.append(f"{candidate}（当前正在运行，请手动删除）")
            continue
        if not looks_like_bupt(candidate):
            continue

        parent = _os.path.dirname(candidate) or "."
        if not _os.access(parent, _os.W_OK):
            skipped.append(f"{candidate}（需要 sudo，请手动删除）")
            continue
        if not dry_run:
            try:
                _os.remove(candidate)
            except OSError as exc:
                skipped.append(f"{candidate}（删除失败：{exc}）")
                continue
        removed.append(candidate)

    # 2. Data directories: honour the same env overrides the rest of the CLI
    #    uses, so a relocated credential/cache dir is not left behind.
    from bupt_cli.credentials import config_dir
    from bupt_cli.session import cache_dir

    data_dirs: list[Path] = []
    for resolver in (config_dir, cache_dir):
        try:
            path = resolver()
        except Exception:
            continue
        if path not in data_dirs:
            data_dirs.append(path)

    home = Path.home().resolve()
    cwd = Path.cwd().resolve()
    data_markers = ("credentials.json", "session.json", "config.json")

    def refuse_reason(target: Path) -> str:
        """Why deleting ``target`` is unsafe, or "" when it is fine.

        ``config_dir()`` honours BUPT_CLI_CONFIG_DIR, so a typo there could
        otherwise point this at $HOME (or /) and wipe far more than intended.
        """
        try:
            resolved = target.resolve()
        except OSError:
            return "路径无法解析"
        if resolved == Path(resolved.anchor):
            return "拒绝删除根目录"
        if resolved == home or resolved in home.parents:
            return "拒绝删除家目录或其上级目录"
        if resolved == cwd or resolved in cwd.parents:
            return "拒绝删除当前目录或其上级目录"
        if not any((resolved / name).is_file() for name in data_markers):
            return "目录里没有 bupt 的数据文件"
        # A marker file alone is not enough: `config.json` / `credentials.json`
        # are common names, so a shared or project directory could contain one.
        # Only own the directory when everything inside is ours.
        try:
            strangers = [
                entry.name
                for entry in resolved.iterdir()
                if entry.name not in data_markers
                and entry.name not in (".DS_Store",)
                and not entry.name.endswith(".tmp")
            ]
        except OSError:
            return "目录无法读取"
        if strangers:
            preview = ", ".join(sorted(strangers)[:3])
            return f"目录里还有其它文件（{preview}），拒绝整体删除"
        return ""

    for data_dir in data_dirs:
        if not data_dir.exists():
            continue
        if getattr(args, "keep_data", False):
            skipped.append(f"{data_dir}（保留；--keep-data）")
            continue
        # A symlink is not a directory we own: unlink the link itself and leave
        # its target alone. `rmtree` refuses symlinks, and swallowing that error
        # used to report "removed" while nothing had been deleted.
        if data_dir.is_symlink():
            if not dry_run:
                try:
                    data_dir.unlink()
                except OSError as exc:
                    skipped.append(f"{data_dir}（删除失败：{exc}）")
                    continue
            removed.append(f"{data_dir}（符号链接）")
            continue

        reason = refuse_reason(data_dir)
        if reason:
            skipped.append(f"{data_dir}（跳过：{reason}）")
            continue
        if not dry_run:
            _shutil.rmtree(data_dir, ignore_errors=True)
            if data_dir.exists():
                skipped.append(f"{data_dir}（删除失败，仍有残留）")
                continue
        removed.append(str(data_dir))

    return {
        "ok": True,
        "dry_run": dry_run,
        "removed": removed,
        "skipped": skipped,
        "note": "未做任何修改" if dry_run else "卸载完成",
    }

