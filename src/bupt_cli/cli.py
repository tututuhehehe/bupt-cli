"""Command-line interface for the ``bupt`` tool.

Every command writes a single JSON document to stdout; failures write a JSON
envelope to stderr and use a stable exit code (2 usage, 3 auth, 4 network,
5 parse/upstream, 6 submit not verified, 7 verification error).
"""

from __future__ import annotations

import argparse
import logging
import math
import sys
from typing import Any, Optional

from bupt_cli import commands
from bupt_cli._vendor.campus_api.exceptions import CampusError
from bupt_cli.config import dorm_config_path
from bupt_cli.credentials import load_credentials
from bupt_cli.errors import (
    CliError,
    UsageError,
    error_envelope,
    from_campus_error,
)
from bupt_cli.output import emit, emit_error
from bupt_cli.probe import run_probe


class JsonArgumentParser(argparse.ArgumentParser):
    """ArgumentParser that reports usage errors as JSON (via UsageError)."""

    def error(self, message: str):  # type: ignore[override]
        raise UsageError(message, hint=f"run `{self.prog} --help` for usage")


def _int_at_least(minimum: int, label: str = "value"):
    """Argparse type for an integer lower bound (usage error otherwise)."""

    def parse(value: str) -> int:
        try:
            number = int(value)
        except (TypeError, ValueError):
            raise argparse.ArgumentTypeError(f"{label} must be an integer") from None
        if number < minimum:
            raise argparse.ArgumentTypeError(f"{label} must be >= {minimum}")
        return number

    return parse


def _float_at_least(minimum: float, label: str = "value"):
    """Argparse type for a finite float lower bound (usage error otherwise)."""

    def parse(value: str) -> float:
        try:
            number = float(value)
        except (TypeError, ValueError):
            raise argparse.ArgumentTypeError(f"{label} must be a number") from None
        if not math.isfinite(number) or number < minimum:
            raise argparse.ArgumentTypeError(f"{label} must be >= {minimum}")
        return number

    return parse


def _positive_float(value: str) -> float:
    """Argparse type for a finite float greater than zero."""

    number = _float_at_least(0.0, "timeout")(value)
    if number == 0.0:
        raise argparse.ArgumentTypeError("timeout must be > 0")
    return number


_POSITIVE_INT = _int_at_least(1, "value")
_NON_NEGATIVE_INT = _int_at_least(0, "value")
_POSITIVE_FLOAT = _positive_float


def _numeric_id(value: str) -> str:
    """Argparse type for upstream ids (numeric strings)."""
    text = str(value).strip()
    if not text.isdigit():
        raise argparse.ArgumentTypeError("id must be numeric")
    return text


def _common() -> argparse.ArgumentParser:
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument(
        "--pretty",
        action="store_true",
        default=argparse.SUPPRESS,
        help="indent JSON output",
    )
    common.add_argument(
        "--timeout",
        type=_POSITIVE_FLOAT,
        default=argparse.SUPPRESS,
        help="HTTP timeout in seconds (default: 20)",
    )
    return common


def build_parser() -> JsonArgumentParser:
    common = _common()
    from bupt_cli import __version__

    parser = JsonArgumentParser(
        prog="bupt",
        description="Agent-friendly CLI for unofficial BUPT campus APIs",
        parents=[common],
        add_help=True,
    )
    parser.add_argument(
        "--version",
        action="version",
        version=f"bupt {__version__}",
        help="print the version and exit",
    )
    sub = parser.add_subparsers(
        dest="command", title="commands", metavar="<command>", parser_class=JsonArgumentParser
    )

    login = sub.add_parser(
        "login",
        parents=[common],
        help="save credentials to ~/.bupt-cli and authenticate",
        description="Save your BUPT account to ~/.bupt-cli/credentials.json "
        "(mode 0600) and authenticate. With no arguments it asks for the "
        "account, password and role interactively; flags allow scripting.",
    )
    login.add_argument(
        "--user",
        metavar="ID",
        help="student id (prompted when omitted)",
    )
    login.add_argument(
        "--password",
        metavar="PASS",
        help="CAS password (prompted, hidden, when omitted). Prefer the prompt: "
        "a flag lands in your shell history",
    )
    login.add_argument(
        "--role",
        metavar="ROLE",
        help="\"graduate\" or \"undergraduate\" (prompted when omitted)",
    )
    login.add_argument(
        "--force",
        action="store_true",
        help="ignore the cached session and authenticate again",
    )
    login.add_argument(
        "--jwgl",
        action="store_true",
        help="also try the mobile JWGL login (undergraduate academic system "
        "only — graduate ids do not exist there; for undergraduates the JWGL "
        "password may differ from the CAS password)",
    )

    status = sub.add_parser(
        "status",
        parents=[common],
        help="show cached session validity (no secrets)",
    )
    status.add_argument(
        "--check",
        action="store_true",
        help="perform a read-only live check (never submits credentials)",
    )

    sub.add_parser(
        "logout",
        parents=[common],
        help="delete the cached session",
    )

    homework = sub.add_parser(
        "homework",
        parents=[common],
        help="UCloud assignments",
        description="Query UCloud (apiucloud.bupt.edu.cn) assignments.",
    )
    homework_sub = homework.add_subparsers(
        dest="homework_command",
        title="homework commands",
        metavar="<subcommand>",
        parser_class=JsonArgumentParser,
    )
    hw_list = homework_sub.add_parser(
        "list",
        parents=[common],
        help="list assignments (all courses by default)",
        description="List assignments across courses, sorted by deadline. "
        "--new N narrows it to assignments published within N days.",
    )
    hw_list.add_argument(
        "--undone",
        action="store_true",
        help="only not-submitted assignments (server-side undone endpoint)",
    )
    hw_list.add_argument("--course", metavar="ID", help="filter by UCloud course/site id")
    hw_list.add_argument(
        "--new",
        type=_NON_NEGATIVE_INT,
        metavar="DAYS",
        help="only assignments published within DAYS days (resolves publish "
        "times through the detail endpoint when the list omits them)",
    )
    hw_list.add_argument(
        "--limit", type=_NON_NEGATIVE_INT, default=0, help="max results (0 = all)"
    )

    hw_show = homework_sub.add_parser(
        "show",
        parents=[common],
        help="assignment detail + attachments",
    )
    hw_show.add_argument("id", metavar="ID", help="assignment/activity id")
    hw_show.add_argument(
        "--course",
        type=_numeric_id,
        metavar="SITE_ID",
        help="only look up the submission time in this course/site id",
    )

    hw_dl = homework_sub.add_parser(
        "download",
        parents=[common],
        help="download teacher attachments for one assignment",
        description="Resolve preview URLs and save teacher attachments to --dir. "
        "Does not submit anything.",
    )
    hw_dl.add_argument("id", metavar="ID", help="assignment/activity id")
    hw_dl.add_argument(
        "--dir",
        default=".",
        metavar="DIR",
        help="output directory (default: current directory)",
    )
    hw_dl.add_argument(
        "--force",
        action="store_true",
        help="overwrite existing files",
    )

    hw_submit = homework_sub.add_parser(
        "submit",
        parents=[common],
        help="upload files/text and submit an assignment (gated)",
        description=(
            "Upload local files and/or text to UCloud, then submit. "
            "NEVER auto-submits: use --dry-run to preview, and "
            "--i-confirm-submit for a real submit (which replaces any previous "
            "submission)."
        ),
    )
    hw_submit.add_argument("id", metavar="ID", help="assignment/activity id")
    hw_submit.add_argument(
        "--file",
        action="append",
        default=[],
        metavar="PATH",
        help="local file to upload (repeatable)",
    )
    hw_submit.add_argument(
        "--text",
        default="",
        metavar="CONTENT",
        help="assignment text content (HTML/plain as accepted by UCloud; "
        "verification compares its plain-text form)",
    )
    hw_submit.add_argument(
        "--resource-id",
        action="append",
        default=[],
        metavar="ID",
        dest="resource_id",
        help="already-uploaded UCloud resource id (repeatable)",
    )
    hw_submit.add_argument(
        "--dry-run",
        action="store_true",
        help="validate inputs and show planned payload; no upload, no submit",
    )
    hw_submit.add_argument(
        "--i-confirm-submit",
        action="store_true",
        help="required to actually POST /ykt-site/work/submit",
    )
    hw_submit.add_argument(
        "--no-verify",
        action="store_true",
        help="skip the read-only post-submit verification",
    )

    hw_verify = homework_sub.add_parser(
        "verify",
        parents=[common],
        help="read-only check that a submission is visible upstream",
        description="Check submit-view and the per-course student list for a "
        "submission. This command never submits and never calls a write "
        "endpoint.",
    )
    hw_verify.add_argument("id", metavar="ID", help="assignment/activity id")
    hw_verify.add_argument(
        "--resource-id",
        action="append",
        default=[],
        metavar="RID",
        dest="resource_id",
        help="expected attachment resource id (repeatable; default: expect none)",
    )
    hw_verify.add_argument(
        "--text",
        default=None,
        metavar="TEXT",
        help="expected submitted text (plain text, whitespace-insensitive); "
        "enables the content check",
    )

    courses = sub.add_parser(
        "courses",
        parents=[common],
        help="current UCloud courses",
        description="Every enrolled course site (all pages fetched internally).",
    )

    schedule = sub.add_parser(
        "schedule",
        parents=[common],
        help="timetable: today / this week / any date (cached)",
        description="Course timetable, served from a local cache. Works for "
        "both account types: graduate (yjxt) and undergraduate (mobile JWGL). "
        "The first run fetches and caches the whole term; later queries are "
        "instant. Use `bupt schedule update` to refresh.",
    )
    schedule.add_argument(
        "--today", action="store_true", help="only today's classes"
    )
    schedule.add_argument(
        "--tomorrow", action="store_true", help="only tomorrow's classes"
    )
    schedule.add_argument("--date", metavar="YYYY-MM-DD", help="classes on one date")
    schedule.add_argument(
        "--week", type=_POSITIVE_INT, metavar="N", help="classes in teaching week N"
    )
    schedule.add_argument(
        "--raw",
        action="store_true",
        help="dump the cached course grid instead of resolved dates",
    )
    schedule_sub = schedule.add_subparsers(
        dest="schedule_command",
        title="schedule commands",
        metavar="<subcommand>",
        parser_class=JsonArgumentParser,
    )
    sched_update = schedule_sub.add_parser(
        "update",
        parents=[common],
        help="refresh the cached timetable for this term",
        description="Fetch the whole term again and rewrite "
        "~/.bupt-cli/timetable.json. Graduate accounts drive a headless "
        "Chrome (4-8s); undergraduate accounts call the mobile JWGL API.",
    )
    sched_export = schedule_sub.add_parser(
        "export",
        parents=[common],
        help="export the cached timetable as an .ics file",
        description="Write one VEVENT per class occurrence to ~/Downloads by "
        "default.",
    )
    sched_export.add_argument("--out", metavar="PATH", help="output file")
    sched_export.add_argument(
        "--no-open-folder",
        action="store_true",
        help="do not reveal the file in Finder",
    )
    schedule_sub.add_parser(
        "calendar",
        parents=[common],
        help="teaching-week -> date table used by the cache",
        description="Show which real dates belong to each teaching week. "
        "Handy for checking that the cached term is the right one.",
    )
    schedule_sub.add_parser(
        "exams",
        parents=[common],
        help="exam arrangements (graduate system; often empty)",
    )

    sub.add_parser(
        "exams",
        parents=[common],
        help="exam weeks/arrangements from mobile JWGL (experimental)",
    )

    scores = sub.add_parser(
        "scores",
        parents=[common],
        help="transcript/GPA from mobile JWGL (experimental)",
    )
    scores.add_argument("--semester", help="filter by semester, e.g. 2025-2026-2")

    notices = sub.add_parser(
        "notices",
        parents=[common],
        help="information-portal notices (experimental; portal often unreachable)",
    )
    notices.add_argument(
        "--type",
        help="comma-separated notice,news,rules,ways (default: notice,news)",
    )
    notices.add_argument(
        "--count",
        type=_NON_NEGATIVE_INT,
        default=10,
        help="max items (default: 10)",
    )
    notices_sub = notices.add_subparsers(
        dest="notices_command",
        title="notices commands",
        metavar="<subcommand>",
        parser_class=JsonArgumentParser,
    )
    notice_show = notices_sub.add_parser(
        "show",
        parents=[common],
        help="notice detail by URL",
    )
    notice_show.add_argument("url", help="notice URL")
    notice_show.add_argument(
        "--max-chars",
        type=_NON_NEGATIVE_INT,
        default=0,
        help="truncate content_text to N characters (0 = no limit)",
    )

    sub.add_parser(
        "card",
        parents=[common],
        help="campus card balance from the portal (experimental)",
    )

    electricity = sub.add_parser(
        "electricity",
        parents=[common],
        help="dorm electricity balance (override the built-in placeholder dorm)",
        description="Query dorm electricity balance. With no arguments the "
        "built-in placeholder dorm (西土城 学一楼 101) is used. Set your own dorm with "
        "--building/--room, BUPT_DORM_* environment variables, or "
        f"{dorm_config_path()} ({{\"dorm\": {{...}}}}). Listing "
        "buildings moved to `bupt electricity buildings`.",
    )
    electricity.add_argument(
        "--building",
        help="building name, e.g. 学五楼, 学5, SHAHE_A or 沙河校区雁北园A楼 "
        "(default: placeholder 学一楼; override via BUPT_DORM_BUILDING or config.json)",
    )
    electricity.add_argument(
        "--room",
        help="room number, e.g. 101 "
        "(default: placeholder 101; override via BUPT_DORM_ROOM or config.json)",
    )
    electricity.add_argument(
        "--floor",
        help="floor override, e.g. 1 or 1层 "
        "(default: inferred from room; override via BUPT_DORM_FLOOR or config.json)",
    )
    electricity.add_argument(
        "--threshold",
        type=_float_at_least(0.0, "threshold"),
        default=10.0,
        help="low-balance threshold in CNY (default: 10)",
    )
    # Hidden legacy flag: accepted by the parser so we can emit a clear usage
    # error instead of argparse's confusing "invalid choice: 'shahe'" for the
    # <subcommand> positional. `--area` belongs to `electricity buildings`.
    electricity.add_argument(
        "--area",
        default=argparse.SUPPRESS,
        help=argparse.SUPPRESS,
    )
    electricity_sub = electricity.add_subparsers(
        dest="electricity_command",
        title="electricity commands",
        metavar="<subcommand>",
        parser_class=JsonArgumentParser,
    )
    elec_buildings = electricity_sub.add_parser(
        "buildings",
        parents=[common],
        help="list registered buildings for an area",
    )
    elec_buildings.add_argument(
        "--area",
        default="shahe",
        help="area for building listing: shahe or xitucheng (default: shahe)",
    )

    classrooms = sub.add_parser(
        "classrooms",
        parents=[common],
        help="real-time empty classrooms from mobile JWGL (experimental)",
    )
    classrooms.add_argument(
        "--campus",
        default="04",
        help="01 = 西土城, 04 = 沙河 (default: 04)",
    )

    update = sub.add_parser(
        "update",
        parents=[common],
        help="check for and install a newer bupt release",
        description="Compare the running version with the latest GitHub release, "
        "then replace the installed binary in place when a newer one exists.",
    )
    update.add_argument(
        "--check",
        action="store_true",
        help="only report whether an update exists",
    )
    update.add_argument(
        "--repo",
        default=None,
        metavar="OWNER/REPO",
        help="GitHub repository to pull releases from",
    )

    uninstall = sub.add_parser(
        "uninstall",
        parents=[common],
        help="remove installed binaries and (optionally) ~/.bupt-cli",
        description="Delete bupt copies found on PATH and ~/.bupt-cli, so "
        "nothing is left behind. Pass --keep-data to preserve your saved "
        "credentials and session.",
    )
    uninstall.add_argument(
        "--keep-data",
        action="store_true",
        help="keep ~/.bupt-cli (by default it is deleted with the binary)",
    )
    uninstall.add_argument(
        "--dry-run",
        action="store_true",
        help="only list what would be removed",
    )

    library = sub.add_parser(
        "library",
        parents=[common],
        help="library catalogue search and holdings (opac.bupt.edu.cn, anonymous)",
        description="Search the library catalogue and read per-copy holdings. "
        "Anonymous: no credentials are read or sent, so it works for any "
        "student regardless of degree level.",
    )
    library_sub = library.add_subparsers(
        dest="library_command",
        title="library commands",
        metavar="<subcommand>",
        parser_class=JsonArgumentParser,
    )
    lib_search = library_sub.add_parser(
        "search",
        parents=[common],
        help="search books by title",
        description="Search the catalogue. Prints total, page info and one "
        "record per book (id, title, authors, isbn, call number, copies).",
    )
    lib_search.add_argument("keyword", metavar="KEYWORD", help="search keyword")
    lib_search.add_argument(
        "--page", type=_int_at_least(1), default=1, help="page number (default: 1)"
    )
    lib_search.add_argument(
        "--page-size",
        type=_int_at_least(1),
        default=10,
        help="records per page (default: 10)",
    )
    lib_search.add_argument(
        "--location",
        default="all",
        help="campus filter: all (default), 本部校区, 沙河校区, 宏福校区",
    )
    lib_search.add_argument(
        "--with-ebook",
        action="store_true",
        help="also mark which records have online full text (one extra request "
        "per record, about 0.06s each)",
    )
    lib_hold = library_sub.add_parser(
        "hold",
        parents=[common],
        help="per-copy holdings for one book id",
        description="Read the holdings table of one record (id comes from "
        "`bupt library search`). Shows department, barcode and status per copy.",
    )
    lib_hold.add_argument(
        "id", metavar="RECCTRLID", help="book id (recCtrlId) from `library search`"
    )

    lib_ebook = library_sub.add_parser(
        "ebook",
        parents=[common],
        help="electronic resources (online full text) for one book id",
        description="Read the \"其他电子资源\" tab: links to online full text "
        "when the library has one. Coverage is much higher for English-language "
        "titles than for older Chinese textbooks.",
    )
    lib_ebook.add_argument(
        "id", metavar="RECCTRLID", help="book id (recCtrlId) from `library search`"
    )

    probe = sub.add_parser(
        "probe",
        parents=[common],
        help="live health matrix of all known endpoints",
        description="Run a live health check of known BUPT endpoints and print a "
        "status matrix. Probe never submits credentials (uses cached sessions).",
    )
    probe.add_argument(
        "--only",
        help="comma-separated groups: " + ", ".join(
            ["public", "cas", "electricity", "portal", "ucloud", "mobile_jwgl", "jwgl_dom", "other"]
        ),
    )
    probe.add_argument(
        "--request-timeout",
        dest="probe_timeout",
        type=_POSITIVE_FLOAT,
        default=8.0,
        help="per-request timeout for probe (default: 8)",
    )

    return parser


def _dispatch(args: argparse.Namespace) -> dict[str, Any]:
    command = getattr(args, "command", None)
    if not command:
        raise UsageError("no command given", hint="run `bupt --help` for commands")

    if command == "login":
        return commands.cmd_login(args)
    if command == "status":
        return commands.cmd_status(args)
    if command == "logout":
        return commands.cmd_logout(args)
    if command == "homework":
        sub = getattr(args, "homework_command", None)
        if sub == "list":
            return commands.cmd_homework_list(args)
        if sub == "show":
            return commands.cmd_homework_show(args)
        if sub == "download":
            return commands.cmd_homework_download(args)
        if sub == "submit":
            return commands.cmd_homework_submit(args)
        if sub == "verify":
            return commands.cmd_homework_verify(args)
        raise UsageError(
            "homework requires a subcommand",
            hint="choose one of: list, show, download, submit, verify",
        )
    if command == "courses":
        return commands.cmd_courses(args)
    if command == "schedule":
        return commands.cmd_schedule(args)
    if command == "exams":
        return commands.cmd_exams(args)
    if command == "scores":
        return commands.cmd_scores(args)
    if command == "notices":
        if getattr(args, "notices_command", None) == "show":
            return commands.cmd_notices_show(args)
        return commands.cmd_notices_list(args)
    if command == "card":
        return commands.cmd_card(args)
    if command == "electricity":
        if getattr(args, "electricity_command", None) == "buildings":
            return commands.cmd_electricity_buildings(args)
        if getattr(args, "area", None) is not None:
            raise UsageError(
                "--area is only valid for `bupt electricity buildings`",
                hint="try `bupt electricity buildings --area shahe` or "
                "`bupt electricity buildings --area xitucheng`",
            )
        return commands.cmd_electricity(args)
    if command == "classrooms":
        return commands.cmd_classrooms(args)
    if command == "uninstall":
        return commands.cmd_uninstall(args)
    if command == "update":
        return commands.cmd_update(args)
    if command == "library":
        sub = getattr(args, "library_command", None)
        if sub == "search":
            return commands.cmd_library_search(args)
        if sub == "hold":
            return commands.cmd_library_hold(args)
        if sub == "ebook":
            return commands.cmd_library_ebook(args)
        raise UsageError(
            "library needs a subcommand",
            hint="try `bupt library search 机器学习`, `bupt library hold <id>` or `bupt library ebook <id>`",
        )
    if command == "probe":
        return run_probe(args)
    raise UsageError(f"unknown command '{command}'")


# The JWGL (学业/教务) backends below are undergraduate-scoped: a graduate
# student id does not exist in them at all. Retrying a "wrong password" banner
# can lock the account, so the hint must not read as "your password is wrong".
JWGL_AUTH_HINT = (
    "the JWGL backends (jwgl.bupt.edu.cn / jwglweixin.bupt.edu.cn) are the "
    "undergraduate academic system; graduate-student ids do not exist there. "
    "If you are a graduate student this command cannot work for your account — "
    "do NOT retry with different passwords (repeated failures can lock the "
    "account). Undergraduate accounts use the same 统一身份认证 password as "
    "CAS, so `bupt login --jwgl` is enough."
)


def _is_jwgl_auth_failure(message: str) -> bool:
    """True when the upstream rejected a JWGL login or no JWGL session exists."""
    lower = message.lower()
    return (
        "jwgl" in lower
        and ("invalid username or password" in lower
             or "该帐号不存在或密码错误" in message
             or "no cached mobile jwgl session" in lower)
    )


def _network_hint(message: str) -> str:
    lower = message.lower()
    if "my.bupt.edu.cn" in lower:
        return (
            "my.bupt.edu.cn is unreachable from this network; see API_STATUS.md "
            "for probe evidence"
        )
    if "jwgl.bupt.edu.cn" in lower or "jwxt.bupt.edu.cn" in lower:
        return (
            "the legacy JWGL host is often unreachable off-campus (TLS reset/timeout); "
            "use `bupt schedule` (graduate system, works off-campus) or see API_STATUS.md"
        )
    return ""


def _quiet_vendor_logging() -> None:
    # The vendored SDK logs fallback warnings; stdout/stderr must stay
    # machine-readable JSON only.
    logging.getLogger("bupt_cli._vendor.campus_api").setLevel(logging.ERROR)
    logging.getLogger("bupt_cli").setLevel(logging.ERROR)


def _known_secrets() -> list[str]:
    """Best-effort secret list for redacting error messages."""
    try:
        return load_credentials(required=False).secret_values()
    except Exception:
        return []


def _verification_stderr_hint(result: dict[str, Any]) -> str:
    """One-line hint for failed/error verification (stdout stays JSON)."""
    verification = result.get("verification")
    if not isinstance(verification, dict):
        return ""
    if verification.get("status") not in ("failed", "error"):
        return ""
    hint = verification.get("hint")
    return hint if isinstance(hint, str) else ""


def _command_names() -> set[str]:
    """Top-level subcommand names, read from the live parser."""
    parser = build_parser()
    for action in parser._actions:  # argparse exposes no public API for this
        if isinstance(action, argparse._SubParsersAction):
            return set(action.choices)
    return set()


def _rewrite_help(argv: list[str]) -> list[str]:
    """Map `bupt help [command]` onto argparse's `--help`.

    ``help`` is a convenience alias for agents and humans alike: ``bupt help``
    shows the overview and ``bupt help homework`` shows that subcommand. Any
    further arguments (``bupt help homework list``) are passed through so
    argparse can resolve the nested parser.
    """
    if not argv or argv[0] != "help":
        return argv
    rest = [arg for arg in argv[1:] if arg not in ("--pretty",)]
    if not rest:
        return ["--help"]
    # Validate the topic against the parser itself: a hardcoded command list
    # silently rejects every newly added command.
    if rest[0] not in ("-h", "--help") and rest[0] not in _command_names():
        raise UsageError(
            f"unknown command for help: {rest[0]!r}",
            hint="run `bupt help` for the overview",
        )
    if len(rest) == 1:
        return [rest[0], "--help"]
    if rest[1].startswith("-"):
        return rest
    return [*rest[:2], "--help"]


def main(argv: Optional[list[str]] = None) -> int:
    _quiet_vendor_logging()
    parser = build_parser()
    argv = list(sys.argv[1:] if argv is None else argv)
    pretty = "--pretty" in argv
    try:
        argv = _rewrite_help(argv)
        args = parser.parse_args(argv)
        pretty = bool(getattr(args, "pretty", pretty))
        result = _dispatch(args)
        if not isinstance(result, dict):
            result = {"result": result}
        exit_code = 0
        if "_exit_code" in result:
            try:
                exit_code = int(result.pop("_exit_code"))
            except (TypeError, ValueError):
                exit_code = 0
        payload = result if "ok" in result else {"ok": True, **result}
        emit(payload, pretty=pretty)
        hint = _verification_stderr_hint(result)
        if exit_code and hint:
            print(hint, file=sys.stderr)
        return exit_code
    except CliError as exc:
        envelope, code = error_envelope(exc, _known_secrets())
        emit_error(envelope, pretty=pretty)
        return code
    except CampusError as exc:
        cli_exc = from_campus_error(exc)
        envelope, code = error_envelope(cli_exc, _known_secrets())
        hint = _network_hint(cli_exc.message)
        if not hint and _is_jwgl_auth_failure(cli_exc.message):
            hint = JWGL_AUTH_HINT
        if hint:
            envelope["error"]["hint"] = hint
        emit_error(envelope, pretty=pretty)
        return code
    except KeyboardInterrupt:
        emit_error(
            {"ok": False, "error": {"type": "interrupted", "message": "interrupted"}},
            pretty=pretty,
        )
        return 130
    except SystemExit:
        raise
    except Exception as exc:  # pragma: no cover - last-resort guard
        envelope, code = error_envelope(exc, _known_secrets())
        emit_error(envelope, pretty=pretty)
        return code


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
