"""Implementations of the ``bupt`` subcommands."""

from __future__ import annotations

import datetime as dt
import math
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
    load_seen_ids,
    parse_datetime,
    parse_since,
    publish_time,
    resolve_attachment_urls,
    save_seen_ids,
    status_for,
    submit_homework,
    upload_homework_file,
)
from bupt_cli.session import (
    BuptSession,
    delete_session,
    seen_state_path,
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
    since = parse_since(args.since) if args.since else None
    session = _session(args)
    client = _client(session)
    try:
        items, method = _load_assignments(client, args)
        _enrich_undone_courses(client, items, args)
        selected = filter_assignments(
            items,
            since=since,
            course_id=args.course,
            undone_only=args.undone,
        )
        if args.limit and args.limit > 0:
            selected = selected[: args.limit]
        return {
            "count": len(selected),
            "method": method,
            "filters": {
                "undone": bool(args.undone),
                "course": args.course,
                "since": since.isoformat(sep=" ") if since else None,
            },
            "assignments": [assignment_dict(item) for item in selected],
        }
    finally:
        _persist(session)


def cmd_homework_new(args) -> dict[str, Any]:
    if args.days is not None and args.days < 0:
        raise UsageError("--days must be >= 0")
    days = 7 if args.days is None else args.days
    cutoff = dt.datetime.now() - dt.timedelta(days=days)

    session = _session(args)
    client = _client(session)
    try:
        items = _dedupe(
            list(
                client.get_all_homework(
                    site_limit=args.max_courses, page_size=args.page_size
                )
            )
        )
        # The student list often leaves assignmentBeginTime empty while the
        # detail endpoint carries the real publish time. Fill it in for a
        # bounded number of items before deciding which mode to use.
        published: dict[str, dt.datetime] = {}
        if not any(publish_time(item) for item in items):
            for item in items[: getattr(args, "detail_lookups", 30)]:
                if not item.activity_id:
                    continue
                try:
                    detail = client.get_homework_detail(item.activity_id)
                except Exception:
                    continue
                parsed = parse_datetime(
                    detail.raw.get("assignmentBeginTime")
                    or detail.raw.get("publishTime")
                    or detail.raw.get("createTime")
                )
                if parsed is not None:
                    published[item.activity_id] = parsed

        def _publication(item):
            return published.get(item.activity_id) or publish_time(item)

        publish_available = bool(published) or any(publish_time(item) for item in items)
        if publish_available:
            method = "publish_time"
            selected = [
                item
                for item in items
                if _publication(item) is not None and _publication(item) >= cutoff
            ]
            note = (
                "filtered by upstream publish/start time (assignmentBeginTime, "
                "resolved via the detail endpoint when the list omits it); items "
                "without it are omitted"
            )
        else:
            method = "seen_ids"
            seen = load_seen_ids()
            selected = [
                item for item in items if item.activity_id and item.activity_id not in seen
            ]
            note = (
                "upstream has no publish timestamp on this account; new items are "
                f"assigned by ids not present in {seen_state_path()}"
            )
        # Always keep the fallback state current, including in publish-time mode.
        save_seen_ids([item.activity_id for item in items if item.activity_id])
        selected.sort(
            key=lambda item: (
                _publication(item) is None,
                _publication(item) or dt.datetime.max,
            ),
            reverse=True,
        )
        if args.limit and args.limit > 0:
            selected = selected[: args.limit]
        assignments = []
        for item in selected:
            entry = assignment_dict(item)
            publication = _publication(item)
            if publication is not None:
                entry["published_at"] = publication.isoformat(sep=" ")
            assignments.append(entry)
        return {
            "count": len(selected),
            "method": method,
            "days": days,
            "cutoff": cutoff.isoformat(sep=" "),
            "note": note,
            "assignments": assignments,
        }
    finally:
        _persist(session)


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
    """Validated --verify-wait value (usage error outside 0..120)."""
    raw = getattr(args, "verify_wait", 5.0)
    if raw is None:
        raw = 5.0
    try:
        wait = float(raw)
    except (TypeError, ValueError) as exc:
        raise UsageError("--verify-wait must be a number") from exc
    if not math.isfinite(wait) or not 0.0 <= wait <= 120.0:
        raise UsageError("--verify-wait must be between 0 and 120 seconds")
    return wait


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
        payload = detail_dict(detail, max_chars=args.max_chars)
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
    session = _session(args)
    client = _client(session)
    try:
        courses = client.get_enrolled_courses(size=args.size, page=args.page)
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


def cmd_schedule(args) -> dict[str, Any]:
    target_date: Optional[dt.date] = None
    if args.today:
        target_date = dt.date.today()
    elif args.date:
        try:
            target_date = dt.date.fromisoformat(args.date)
        except ValueError as exc:
            raise UsageError(f"invalid --date '{args.date}' (use YYYY-MM-DD)") from exc

    source = None if args.source == "auto" else args.source
    session = _session(args)
    client = _client(session)
    try:
        overview = client.get_schedule(week=args.week, source=source)
        courses = overview.courses
        if target_date is not None:
            courses = overview.get_day_courses(target_date.weekday())
        return {
            "week": overview.week,
            "dates": overview.dates,
            "source": overview.source,
            "day": target_date.isoformat() if target_date else None,
            "count": len(courses),
            "courses": [_schedule_course(course, overview.dates) for course in courses],
        }
    finally:
        _persist(session)


def cmd_scores(args) -> dict[str, Any]:
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
        notices = client.get_notices(
            count=args.count, types=types, page=args.page
        )
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
    * ``--upload-only`` uploads files and returns resource ids without
      calling ``/work/submit``.
    """
    from pathlib import Path

    files = list(getattr(args, "file", None) or [])
    resource_ids = [str(r) for r in (getattr(args, "resource_id", None) or [])]
    text = getattr(args, "text", None) or ""
    dry_run = bool(getattr(args, "dry_run", False))
    upload_only = bool(getattr(args, "upload_only", False))
    confirm = bool(getattr(args, "i_confirm_submit", False))
    no_verify = bool(getattr(args, "no_verify", False))
    verify_wait = _verify_wait(args)

    if dry_run and upload_only:
        raise UsageError("--dry-run and --upload-only are mutually exclusive")
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

        if upload_only:
            return {
                "upload_only": True,
                "assignment_id": args.id,
                "title": detail.title,
                "uploaded": uploaded,
                "attachment_ids": attachment_ids,
                "note": "files uploaded; /ykt-site/work/submit NOT called",
            }

        if not confirm:
            raise UsageError(
                "refusing to submit without --i-confirm-submit",
                hint="re-run with --i-confirm-submit after reviewing "
                "`bupt homework submit ID --dry-run`, or use --upload-only",
            )

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


def cmd_grad_schedule(args) -> dict[str, Any]:
    """Semester timetable, via the browser."""
    from bupt_cli import grad

    pages = _grad_pages(args, ("schedule",))
    try:
        entries = grad.merge_periods(grad.parse_timetable(pages["schedule"]))
        opts = grad.term_options(pages["schedule"])
    except grad.GradError as exc:
        raise AuthError(str(exc), hint=getattr(exc, "hint", "")) from None
    return {
        "term": opts[0][1] if opts else "",
        "count": len(entries),
        "courses": entries,
    }


def cmd_grad_calendar(args) -> dict[str, Any]:
    """Teaching calendar: teach week -> real dates."""
    from bupt_cli import grad

    pages = _grad_pages(args, ("calendar",))
    if "calendar" not in pages:
        raise AuthError("没能取到教学日历页面")
    term = grad.term_from_html(pages["calendar"])
    try:
        weeks = grad.parse_calendar(pages["calendar"], _calendar_year(grad, term))
    except grad.GradError as exc:
        raise AuthError(str(exc), hint=getattr(exc, "hint", "")) from None
    return {
        "term": term,
        "week_count": len(weeks),
        "weeks": [
            {"week": w, "monday": d[0], "sunday": d[-1], "dates": d}
            for w, d in sorted(weeks.items())
        ],
    }


def cmd_grad_exams(args) -> dict[str, Any]:
    """Exam arrangements (the table is often empty for graduate courses)."""
    from bupt_cli import grad

    pages = _grad_pages(args, ("exams",))
    return grad.parse_exams(pages.get("exams", ""))


def cmd_grad_ics(args) -> dict[str, Any]:
    """Export the timetable as an .ics file into ~/Downloads."""
    from pathlib import Path

    from bupt_cli import grad

    pages = _grad_pages(args, ("schedule", "calendar"))
    if "calendar" not in pages:
        raise AuthError("没能取到教学日历，无法把周次换成日期")

    courses = grad.merge_periods(grad.parse_timetable(pages["schedule"]))
    # The calendar page is the authority for week -> date, so its term wins; the
    # timetables's dropdown label is only a fallback for the calendar name.
    opts = grad.term_options(pages["schedule"])
    calendar_term = grad.term_from_html(pages["calendar"])
    term = calendar_term or (opts[0][1] if opts else "")
    try:
        weeks = grad.parse_calendar(pages["calendar"], _calendar_year(grad, term))
    except grad.GradError as exc:
        raise AuthError(str(exc), hint=getattr(exc, "hint", "")) from None

    content = grad.build_ics(courses, weeks, term=term)
    if args.out:
        out = Path(args.out).expanduser()
    else:
        safe = re.sub(r"[^\w\u4e00-\u9fa5-]+", "-", term).strip("-")
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
        "term": term,
        "courses": len(courses),
        "weeks": len(weeks),
        "events": content.count("BEGIN:VEVENT"),
        "path": str(out),
        "hint": "在日历 App 里双击该文件导入；重复导入会生成重复事件",
    }


def _versions_tuple(text: str) -> tuple[int, ...]:
    parts = re.findall(r"\d+", text or "")
    return tuple(int(p) for p in parts[:3]) or (0,)


def _github_repo_exists(repo: str) -> Optional[bool]:
    """True/False when GitHub answers, None when it cannot be asked."""
    import json as _json
    import urllib.error as _urlerr
    import urllib.request as _urlreq

    request = _urlreq.Request(
        f"https://api.github.com/repos/{repo}",
        headers={"Accept": "application/vnd.github+json", "User-Agent": "bupt-cli"},
    )
    try:
        with _urlreq.urlopen(request, timeout=15) as resp:
            _json.load(resp)
        return True
    except _urlerr.HTTPError as exc:
        if exc.code == 404:
            return False
        return None
    except Exception:
        return None


def cmd_update(args) -> dict[str, Any]:
    """Compare against the latest GitHub release and replace the binary."""
    import json as _json
    import os as _os
    import shutil as _shutil
    import sys as _sys
    import tempfile as _tempfile
    import urllib.error as _urlerr
    import urllib.request as _urlreq
    import zipfile as _zipfile
    from pathlib import Path

    from bupt_cli import __version__

    repo = args.repo or _os.environ.get("BUPT_CLI_REPO") or DEFAULT_REPO
    api = f"https://api.github.com/repos/{repo}/releases/latest"
    request = _urlreq.Request(
        api, headers={"Accept": "application/vnd.github+json", "User-Agent": "bupt-cli"}
    )
    try:
        with _urlreq.urlopen(request, timeout=20) as resp:
            release = _json.load(resp)
    except _urlerr.HTTPError as exc:
        if exc.code == 404:
            # /releases/latest 404s both when the repo is missing and when it
            # has no releases; ask about the repo itself to tell them apart.
            repo_exists = _github_repo_exists(repo)
            if repo_exists is None:
                note = f"无法确认 {repo} 的状态（GitHub 不可达）"
            elif repo_exists:
                note = f"仓库 {repo} 存在，但还没有发布任何 release"
            else:
                note = (
                    f"仓库 {repo} 不存在或未公开；用 --repo OWNER/REPO 指定，"
                    "或设 BUPT_CLI_REPO"
                )
            return {
                "ok": True,
                "current": __version__,
                "latest": None,
                "update_available": False,
                "repo": repo,
                "repo_exists": repo_exists,
                "note": note,
            }
        if exc.code in (403, 429):
            return {
                "ok": True,
                "current": __version__,
                "latest": None,
                "update_available": None,
                "repo": repo,
                "note": "GitHub API 限流，稍后再试；或设置 GITHUB_TOKEN 环境变量",
            }
        raise AuthError(f"检查更新失败：GitHub 返回 {exc.code}") from None
    except Exception as exc:
        raise AuthError(f"检查更新失败：{exc}") from None

    latest = str(release.get("tag_name") or "").lstrip("v")
    newer = _versions_tuple(latest) > _versions_tuple(__version__)

    result: dict[str, Any] = {
        "ok": True,
        "current": __version__,
        "latest": latest or None,
        "update_available": newer,
        "repo": repo,
    }

    if not newer:
        result["note"] = "已经是最新版本"
        return result
    if getattr(args, "check", False):
        result["note"] = "有新版本；去掉 --check 即可安装"
        return result

    # Find a macOS/arm64 asset (zip preferred).
    assets = release.get("assets") or []
    asset = next(
        (
            a
            for a in assets
            if "bupt" in str(a.get("name", "")).lower()
            and str(a.get("name", "")).lower().endswith(".zip")
        ),
        None,
    )
    if asset is None:
        result["note"] = "找到新版本，但 release 里没有可用的 bupt-*.zip 资产"
        result["assets"] = [a.get("name") for a in assets]
        return result

    # Where the running program lives; a frozen binary replaces itself.
    frozen = bool(getattr(_sys, "frozen", False))
    target = Path(_sys.executable).resolve() if frozen else None

    with _urlreq.urlopen(asset["browser_download_url"], timeout=120) as resp:
        payload = resp.read()

    with _tempfile.TemporaryDirectory() as tmp:
        archive = Path(tmp) / "bupt.zip"
        archive.write_bytes(payload)
        with _zipfile.ZipFile(archive) as zf:
            member = next(
                (n for n in zf.namelist() if n.endswith("/bupt") or n == "bupt"), None
            )
            if member is None:
                raise UsageError("下载的压缩包里没有 bupt 可执行文件")
            extracted = Path(tmp) / "bupt"
            extracted.write_bytes(zf.read(member))
        extracted.chmod(0o755)

        if target is not None:
            backup = target.with_suffix(".old")
            _shutil.copy2(target, backup)
            incoming = target.with_suffix(".new")
            _shutil.copy2(extracted, incoming)
            _os.replace(incoming, target)
            result["installed_to"] = str(target)
            result["backup"] = str(backup)
    if target is None:
        # Running from source (`python -m bupt_cli`): there is no binary to
        # replace, so do not claim success.
        result["installed_to"] = None
        result["note"] = (
            f"找到新版本 {latest}，但当前不是独立二进制，未自动安装；"
            "请重新下载或运行 ./build.sh 后再替换"
        )
        return result
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

    # Around the running program (project launcher, ./dist/bupt, ...).
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
        # `python -m bupt_cli` via the launcher: the launcher is $0 of the shell
        # script, which Python does not see. Detect it as the project launcher
        # whose parent holds src/bupt_cli.
        launcher = project_root / "bupt"
        if launcher.exists():
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
    data_markers = ("credentials.json", "session.json", "config.json",
                    "homework-seen.json")

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

