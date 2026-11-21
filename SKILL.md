---
name: bupt-cli
description: >-
  Use this for anything on BUPT (北邮) campus systems: UCloud homework
  (list/show/download/submit/verify), courses, graduate or undergraduate
  timetable/exams/scores/classrooms, library, dorm electricity, notices.
---
# bupt CLI

Run `bupt <command>`. Output is one JSON document with `schema: "bupt.v1"` and `ok` on stdout; errors are one JSON document on stderr with `error.type` (`usage`=2, `auth`=3, `network`=4, `upstream`=5) and the matching exit code. `--pretty` only reformats. Run `bupt help <command>` for full flags.

Credentials come only from `~/.bupt-cli/credentials.json`, created by `bupt login --role graduate|undergraduate` (the id and password prompt interactively when not passed as flags; `BUPT_USERNAME`-style env vars are ignored on purpose). Sessions are cached and reused; only an explicit auth error means the user must re-run `bupt login`.

## Common commands
- `bupt courses`: UCloud course sites.
- `bupt homework [--undone] [--course ID] [--new DAYS]`: assignment list (bare `homework`, no `list` subcommand).
- `bupt homework show ID`: detail, attachments, `submitted`/`submitted_at`.
- `bupt homework download ID --dir DIR`: teacher attachments into the given directory only.
- `bupt homework verify ID [--resource-id RID ...]`: read-only check that a submission is visible.
- `bupt schedule --today | --week N`: reads the local snapshot in milliseconds; `bupt schedule update` refreshes it (graduate = direct HTTPS, ~0.8 s; no browser needed). `schedule calendar` maps teaching weeks to dates; `schedule export` writes an `.ics`.
- `bupt exams`: routed by role; a successful empty result is reported as `count: 0`, never as an error.
- `bupt scores` / `bupt classrooms --campus 01|04`: undergraduate only.
- `bupt library search KEYWORD` / `hold ID` / `ebook ID`: anonymous, no login needed.
- `bupt electricity --building B --room R`: dorm balance; set the user's own dorm via flags, `BUPT_DORM_*`, or `~/.bupt-cli/config.json` — the built-in value is a placeholder. `electricity buildings --area shahe|xitucheng` lists buildings.
- `bupt diagnose [--only public,cas,ucloud]`: availability matrix; never submits passwords or writes state.
- `bupt notices` / `notices show URL` / `notices search KEYWORD` / `notices download URL [--dir DIR] [--force]` / `card`: portal over HTTP (`my.bupt.edu.cn` has no HTTPS). Needs a campus route; off-campus they fail with network 4. `search` matches notice titles and returns the first page plus the portal total. `download` saves one notice's attachments into `--dir` (default `.`); it takes the notice page, because the portal serves a file only when the request names a portal page as `Referer`. The saved name is the page's link text, an existing file is skipped unless `--force`, and a captcha/HTML answer exits 5 without writing anything. An attachment link is refused. One response is capped at 8 MiB. `search` returns at most 10 results per page (the portal's page size).
- `bupt update --check`: read-only release check.

## Submitting homework (only with the user's explicit consent)
1. `bupt homework submit ID --file f.pdf --text "..." --dry-run`: preview first — zero uploads, zero writes.
2. Show the user exactly what would be submitted. Only after they explicitly agree: `bupt homework submit ID --file f.pdf --i-confirm-submit`.
3. A confirmed submit uploads and submits at most once. Exit 6 means the outcome is unknown (never resubmit on your own — use `verify`), exit 7 means post-submit verification failed.
4. After any submit, check with `bupt homework show ID` / `verify ID` and report honestly.

## Notes
- Don't re-run `bupt login` in quick succession; repeated failures can lock the account. An auth error on one service does not mean the others are broken.
- Never print credentials, cookies, tickets, or tokens; `bupt status` already shows only sanitized metadata.
- Don't scrape at high frequency; campus endpoints rate-limit and change without notice.
