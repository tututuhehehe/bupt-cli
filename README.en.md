# bupt — BUPT campus API CLI

**English** | [简体中文](README.md)

An unofficial, agent-friendly command-line client for Beijing University of
Posts and Telecommunications campus services. It prints one JSON document per
invocation and stores all state in `~/.bupt-cli/` (mode `0600`).

**Open source, MIT licensed.** No account, password or session is bundled with
the source; run `bupt login` once to configure your own.

## Attribution

This project stands on other people's reverse-engineering work. Endpoints,
protocol details and edge cases were learned from (and in places verified
against) these projects:

| Project | Licence | What this CLI took from it |
|---|---|---|
| [`campus-api`](https://github.com/uarix/BUPT-API-Collection) (vendored under `src/bupt_cli/_vendor/campus_api/`) | MIT | CAS login flow, UCloud BladeX token exchange, electricity and portal clients |
| [`uarix/BUPT-API-Collection`](https://github.com/uarix/BUPT-API-Collection) | MIT (declared; no LICENSE file in repo) | Library OPAC endpoint layout — the `search-full-result.json` / `bookInfo_*.html` shape |
| [`wzekin/bupt-api`](https://github.com/wzekin/bupt-api) | MIT | Early evidence that the CAS form posts the password in clear text behind hidden inputs |
| [`Yokumii/bupt-api-collected`](https://github.com/Yokumii/bupt-api-collected) | Apache-2.0 | Confirmation that the mobile JWGL login is AES-based, not CAS; exam/score endpoint names |

The timetable integration (`bupt schedule`), the library client, the
iCalendar exporter, the headless-CDP driver and the Riversafe workaround were
written for this project. Each was verified live in 2026-11; see
[`API_STATUS.md`](API_STATUS.md) for the evidence.

Please respect each upstream project's licence if you copy from this repo.

## What it does

- **CAS login** (`auth.bupt.edu.cn`) with an UCloud BladeX token exchange
- **UCloud** homework list/show/download, gated submit (`--dry-run` / `--i-confirm-submit`), courses
- **Library catalogue** (`opac.bupt.edu.cn`) search + per-copy holdings,
  **anonymous** — works for undergraduates and graduate students alike
- **Graduate system** (`yjxt.bupt.edu.cn`): semester timetable, teaching
  calendar, exam arrangements, and **`bupt schedule export`** which exports the
  timetable as an iCalendar file with real dates. Sits behind Riversafe, so a
  headless Chrome is driven over CDP to read it
- **Dorm electricity** (沙河 / 西土城) via `app.bupt.edu.cn`; with no
  arguments it uses a built-in placeholder dorm (西土城 学一楼 101);
  configure your own dorm with flags, environment variables, or config.json
- **Mobile JWGL** (`jwglweixin.bupt.edu.cn`, HTTPS): timetable, exams, scores,
  empty classrooms — **undergraduate-only**. These backends (with
  `jwgl.bupt.edu.cn`) are the undergraduate academic system; a graduate-student
  id does not exist in them, so the commands cannot work for graduate accounts
  regardless of password. Undergraduate accounts use the same 统一身份认证
  password as CAS, so `bupt login --jwgl` is all that is needed.
- **Information portal** notices/card balance — implemented but the portal is
  often unreachable from off-campus networks
- **`bupt probe`** — live health matrix for every endpoint known to this
  project
- **`bupt login` / `bupt update` / `bupt uninstall`** — interactive credential
  setup, self-update, and clean removal

See [`API_STATUS.md`](API_STATUS.md) for the verified status of every endpoint
for a snapshot of endpoint behaviour.

## Who this works for (本科生 vs 研究生)

Verified on a graduate account (2026-11):

| Area | Undergraduate | Graduate |
|---|---|---|
| UCloud (`courses`, `homework *`) | ✓ | **✓** |
| Information portal (`notices`, `card`) | ✓ | **✓** |
| Dorm electricity | ✓ | ✓ |
| JWGL (`schedule`, `exams`, `scores`, `classrooms`) | ✓ | **✗ — account does not exist in that system** |

The four JWGL commands go to `jwgl.bupt.edu.cn` / `jwglweixin.bupt.edu.cn`,
which are the *undergraduate* academic system. A graduate-student id is not
present there, so these commands fail with an auth error. The error hint now
says so explicitly — **do not retry with other passwords**, because repeated
failures can lock the account. For graduate accounts, use the
`bupt schedule` commands instead — they query `yjxt.bupt.edu.cn`, the graduate system behind
the portal's "研究生系统" entry.

> Unofficial and not production software. Campus systems may change at any
> time. Do not use this for high-frequency scraping or anything prohibited by
> BUPT network policy.

## Install / run

There is exactly **one entry point**: the `bupt` launcher script in the project
root. It sets `PYTHONPATH` to `src/` and `.deps/` and runs a plain CPython — no
virtualenv, no editable install, no `.pth` indirection, so there is no
"package disappeared" failure mode.

```bash
cd /path/to/bupt-cli
./bupt --help          # works from any cwd too (the dir is on PATH)
```

How it resolves things:

| Item | Location |
|---|---|
| Source | `src/` (loaded directly, never installed) |
| httpx + lxml | `.deps/` (one-time install, independent of any venv) |
| Interpreter | uv-managed CPython 3.14, read-only |

If `.deps/` is ever missing, reinstall it once:

```bash
uv pip install --python ~/.local/share/uv/python/cpython-3.14-macos-aarch64-none/bin/python3.14 \
  --target .deps 'httpx>=0.27.0' 'lxml>=5.0.0'
```

### Standalone binary (macOS arm64)

```bash
./build.sh             # -> dist/bupt  (16 MB, self-contained)
./dist/bupt --version
```

The binary links only against the system `libSystem`/`libz`, so it runs on any
macOS arm64 machine without Python installed. All state still lives in
`~/.bupt-cli/`, so the binary itself carries nothing.

`dist/` is a build artifact and is safe to delete (`./build.sh --clean`).

**This repo does not install itself globally.** If you want `bupt` on your
`PATH`, copy the binary yourself, e.g.:

```bash
cp dist/bupt ~/.local/bin/bupt && chmod 755 ~/.local/bin/bupt
# or, if you prefer a system-wide copy:
sudo cp dist/bupt /usr/local/bin/bupt
```

To undo any of this:

```bash
bupt uninstall             # remove bupt copies on PATH AND ~/.bupt-cli
bupt uninstall --keep-data # keep ~/.bupt-cli (credentials + session)
bupt uninstall --dry-run   # show what would be removed
```

## Credentials

The CLI reads credentials from the environment or `~/.bupt-cli/credentials.json`.
`bupt login` prompts for them and writes that file with mode `0600`.

`--user` / `--password` / `--role` flags exist for scripting, but a password
passed on the command line lands in your shell history and the process list —
prefer the prompt.

1. Environment (highest priority):

   ```bash
   export BUPT_USERNAME=2026xxxxxx
   export BUPT_PASSWORD='...'
   ```

2. `~/.bupt-cli/credentials.json` (mode `0600`):

   ```json
   {
     "username": "2026xxxxxx",
     "password": "CAS password",
     "jwgl_username": "optional, defaults to username",
     "role": "graduate"
   }
   ```

Optional environment variables:

| Variable | Meaning |
|---|---|
| `BUPT_DORM_BUILDING` / `BUPT_DORM_ROOM` / `BUPT_DORM_FLOOR` | default dorm for `bupt electricity` (see below) |
| `BUPT_CLI_CACHE_DIR` | override `~/.bupt-cli` |
| `BUPT_CLI_CONFIG_DIR` | override `~/.bupt-cli` |
| `BUPT_MAX_CAS_LOGINS` | per-process CAS login safety cap (default 5) |

The password, cookies, UCloud token and mobile-JWGL JWT are never printed by
any command. `bupt status` only shows redacted metadata (domains/counts,
boolean presence, expiry times). Treat `~/.bupt-cli/session.json` as a
credential: it contains session cookies and tokens (but no password).

### Default dorm (electricity)

`bupt electricity` with no arguments uses a built-in **placeholder**
dorm (西土城/海淀 学一楼 101). This is only an example — set your own dorm
before querying. Each field (building / room / floor) is resolved
independently, in this order:

1. CLI flag (`--building`, `--room`, `--floor`)
2. environment (`BUPT_DORM_BUILDING`, `BUPT_DORM_ROOM`, `BUPT_DORM_FLOOR`)
3. `~/.bupt-cli/config.json` (`BUPT_CLI_CONFIG_DIR` honoured):

   ```json
   {"dorm": {"building": "学一楼", "room": "101", "floor": "1"}}
   ```

4. the built-in placeholder dorm (`学一楼` / `101`, floor inferred from the room)

The response includes `"source": "flag" | "env" | "config" | "default"`
showing where the **room** came from. If `--building` is given, `--room` is
required as well (exit 2 otherwise). Invalid JSON in `config.json` is also a
usage error (exit 2) so a typo cannot silently switch meters.

Building names accept the upstream xitucheng names and short forms
(`学五楼`, `学5楼`, `学5`, `5`) plus the shahe names/aliases (`SHAHE_A`,
`沙河校区雁北园A楼`). The campus area is inferred from the building name; use
`bupt electricity buildings --area shahe|xitucheng` to list upstream names.
Listing only reads upstream building metadata and deliberately ignores
`config.json`: a broken config blocks queries (exit 2) but never blocks
listing buildings.

## Commands

All commands return compact JSON on stdout. Add `--pretty` anywhere for
indentation. Errors go to stderr as
`{"ok":false,"error":{"type":"...","message":"...","hint":"..."}}`.

| Command | Description |
|---|---|
| `bupt login [--user ID] [--password PASS] [--role ROLE] [--force] [--jwgl]` | Save your account to `~/.bupt-cli/credentials.json` (mode `0600`) and authenticate. With no flags it prompts for id, password (hidden) and role. `--jwgl` additionally tries the mobile-JWGL login (undergraduate system only). |
| `bupt status [--check]` | Show cached session validity without secrets. `--check` performs a read-only live check (ticket issuance + UCloud info); it never submits credentials. |
| `bupt logout` | Delete `~/.bupt-cli/session.json`. |
| `bupt homework list [--undone] [--course ID] [--new DAYS] [--limit N]` | Assignments across courses. `--undone` uses the server-side undone endpoint; `--new DAYS` keeps only recently published ones. |
| `bupt homework show ID [--max-chars N] [--course SITE_ID]` | Assignment detail, submission state (`submitted`, `submit_content`, `submit_attachments`, `submitted_at`), plain-text content and attachment URLs. Teacher attachments are resolved through the UCloud preview endpoint; `--course` limits the submission-time lookup to one course/site id. |
| `bupt homework download ID [--dir DIR] [--force]` | Download teacher attachments into `DIR` (CDN URLs; no Blade-Auth). |
| `bupt homework submit ID [--file PATH]… [--text …] [--resource-id ID]… [--dry-run | --i-confirm-submit] [--no-verify]` | Upload and/or submit. **Never auto-submits**: `--dry-run` plans only; real submit needs `--i-confirm-submit`, and without it nothing is even uploaded. A real submit is then verified with read-only calls (skip with `--no-verify`). |
| `bupt homework verify ID [--resource-id RID]… [--text TEXT]` | Read-only check that a submission is visible upstream (submit-view + student list). Never submits and never calls a write endpoint. |
| `bupt courses` | Current UCloud course sites. |
| `bupt schedule [--week N] [--today] [--date YYYY-MM-DD] [--source auto\|mobile\|dom]` | Timetable. Mobile JWGL first, DOM fallback. Experimental — unavailable with the current credentials/network. |
| `bupt exams` | Exam weeks and arrangements from mobile JWGL. Experimental. |
| `bupt scores [--semester NAME]` | Transcript/GPA from mobile JWGL (`termGPA`). Experimental. |
| `bupt classrooms [--campus 01\|04]` | Real-time empty classrooms from mobile JWGL. Experimental. |
| `bupt notices [--type notice,news,rules,ways] [--count N] [--page P]` | Portal notices. Experimental; portal is often unreachable from off-campus networks. |
| `bupt notices show URL [--max-chars N]` | Portal notice detail. |
| `bupt card` | Campus-card balance widget from the portal. Experimental. |
| `bupt electricity [--building B] [--room R] [--floor F] [--threshold N]` | Dorm electricity balance. With no arguments it uses the built-in placeholder dorm (西土城 学一楼 101); set your own dorm via flag > `BUPT_DORM_*` env > `config.json` > placeholder. Output adds `"source"`. |
| `bupt electricity buildings [--area shahe\|xitucheng]` | List buildings registered on the electricity platform for one campus area. |
| `bupt probe [--only GROUPS] [--request-timeout S]` | Live endpoint health matrix (`public,cas,electricity,portal,ucloud,mobile_jwgl,jwgl_dom,other`). |
| `bupt library search KEYWORD [--page N] [--page-size N] [--location CAMPUS] [--with-ebook]` | Library catalogue search (`opac.bupt.edu.cn`), anonymous. `--with-ebook` marks records that have online full text (one extra request each). |
| `bupt library hold RECCTRLID` | Per-copy holdings for one record: department, barcode, call number, status. |
| `bupt library ebook RECCTRLID` | Electronic-resource links for one record (coverage is skewed to English titles). |
| `bupt schedule` | **This week's timetable**, served from a local cache. `--today` / `--tomorrow` / `--date` / `--week N` for one day or week; `--raw` dumps the grid. Works for both account types. |
| `bupt schedule update [--force]` | Refresh `~/.bupt-cli/timetable.json` (headless Chrome for graduate accounts, mobile JWGL API for undergraduates). |
| `bupt schedule export [--out PATH]` | Export the timetable as an iCalendar file (default `~/Downloads/bupt-<term>.ics`), one event per class occurrence. |
| `bupt schedule calendar` | Teaching-week -> date table. |
| `bupt schedule exams` | Exam arrangements (graduate system; often empty). |
| `bupt update [--check] [--repo OWNER/REPO]` | Compare with the latest GitHub release and replace the installed binary in place (keeping `bupt.old`). |
| `bupt uninstall [--keep-data] [--dry-run]` | Remove bupt binaries found on PATH and `~/.bupt-cli`. `--keep-data` keeps your credentials; `--dry-run` only lists what would go. |
| `bupt help [command]` | Alias for `--help`; `bupt help grad` shows a subcommand's options. |

### Session reuse and lockout safety

`bupt login` caches cookies and tokens. Later commands restore them and only
re-authenticate on `401`/expired tokens. The CLI also:

- persists CAS cookies **immediately** after login, before the UCloud token
  exchange, so a transient token failure does not waste a login;
- refuses more than `BUPT_MAX_CAS_LOGINS` (default 5) credential submissions
  in one process;
- never submits mobile-JWGL credentials from commands other than
  `bupt login --jwgl`; other commands use a cached token or fail fast;
- temporarily blocks repeated failed mobile-JWGL logins for 6 hours (reset
  with `bupt login --jwgl --force`);
- `bupt probe` never submits credentials.

### `homework list` / `--new`

`homework list` returns assignments sorted by deadline. `--undone` switches to
the server-side undone endpoint.

`--new DAYS` keeps only assignments published within `DAYS` days. UCloud's list
record usually leaves `assignmentBeginTime` empty, so the CLI fetches up to 30
assignment details to recover the publish time; the `note` field says so. If no
publish time can be recovered at all, the note says the filter is unavailable
rather than silently falling back to the deadline.

`--limit` bounds the result count.

### `homework show` submission state

`GET /ykt-site/work/detail` carries no information about the student's own
submission, so `homework show` reads it from
`GET /ykt-site/work/submit-view?assignmentId=…` instead. The vendor detail
parser also treats `assignmentStatus: 0` as submitted (`99` = not submitted).

The output therefore includes:

- `submitted` — true when submit-view has a real `assignmentId` (not `-1`)
  or a non-empty `attachmentIds`;
- `submit_content` — submit-view `assignmentContent` as plain text (`null`
  when empty);
- `submit_attachments` — one `{title,url,resource_id,size}` per
  `attachmentIds` entry, with the preview URL resolved through
  `/blade-source/resource/preview-url` and the title taken from its
  `content-disposition` filename;
- `submitted_at` — submit time from `/ykt-site/work/student/list`, looked up
  only when submitted (matching sites by `className`, or only `--course
  SITE_ID` when given); `null` if the lookup fails.

Submit-view failures never fail the command: detail-derived values are kept
and a short redacted `submit_view_error` string is added.

### `homework submit` post-submit verification

A successful `/ykt-site/work/submit` response is not trusted on its own. After
a real submit the CLI immediately verifies the result with **read-only** calls:

1. `GET /ykt-site/work/submit-view?assignmentId=…` — a real submission
   (`assignmentId` not `-1`) with exactly the expected `attachmentIds` set, and
   (when `--text` is non-empty) the expected `assignmentContent` after HTML
   stripping and whitespace normalisation;
2. `GET /ykt-site/work/student/list` for the matching course(s) — a record with
   a non-empty `submitTime` (or `isSubmitted`), whose `submit_time` is reported
   as `submitted_at`.

The content check compares the **plain-text** version of
`assignmentContent` (HTML tags stripped, whitespace collapsed) with the
expected text normalised the same way. If you submitted HTML with `--text`,
pass the visible plain text as the expected `--text` (or omit it to skip the
content check).

Upstream is eventually consistent, so each attempt sleeps 5 seconds
seconds (float, `0`–`120`, default `5`) and there are at most two attempts.
`--no-verify` skips verification. Both flags are accepted with `--dry-run` and
but have no effect there (that path never submits). The same
session/token is reused — verification performs no new login and never calls a
write endpoint.

The submit result gains:

- `verified` — `true` (passed), `false` (not confirmed) or `null` (error /
  skipped);
- `submitted_at` — `submitTime` from the student list, else `null`;
- `verification` — `{status, attempts, wait_seconds, checks, …}` where
  `status` is `passed`, `failed`, `error` or `skipped`; `checks.submit_view`
  and `checks.list` report expected/observed values and any redacted `error`
  (texts truncated to 200 characters); `failed_checks` and a `hint` appear when
  verification fails.

Exit codes: `6` means the submit reported success but the submission could not
be confirmed (stdout still has `ok:true` and `verified:false`, and the hint goes
to stderr); `7` means verification could not run (`verified:null` plus a
top-level `warning`). For agent/skill callers: **a real submit is verified
automatically; exit 6 = not confirmed (check manually, never blindly resubmit),
7 = verification couldn't run.**

`bupt homework verify ID` runs exactly the same read-only checks without
submitting (useful for manual checks and live acceptance): the expected
attachment set is the given `--resource-id`s (none = expect no attachments), and
`--text` enables the content check. It uses the same output block and exit
codes, without the submit-only fields.

## Examples

```bash
uv run bupt login
uv run bupt status --check --pretty
uv run bupt homework list --undone --pretty
uv run bupt homework list --new 3
uv run bupt homework show ASSIGNMENT_ID --pretty
uv run bupt homework show ANOTHER_ASSIGNMENT_ID --course SITE_ID --pretty
uv run bupt homework download ASSIGNMENT_ID --dir ./hw
uv run bupt homework submit ASSIGNMENT_ID --file ./ans.pdf --dry-run --pretty
uv run bupt homework submit ASSIGNMENT_ID --file ./ans.pdf --text '说明' --i-confirm-submit --pretty
uv run bupt homework verify ANOTHER_ASSIGNMENT_ID --resource-id RESOURCE_ID --text '说明' --pretty
uv run bupt courses | jq '.courses[] | {name, teacher}'
uv run bupt electricity --pretty                     # uses placeholder dorm unless you configured your own
uv run bupt electricity --building SHAHE_A --room 102 --pretty
uv run bupt electricity buildings --area xitucheng --pretty
uv run bupt probe --only ucloud,public --pretty
uv run bupt logout
```

Typical output:

```json
{"ok":true,"count":2,"method":"undone","assignments":[{"id":"...","title":"第二章作业","course":"通信网理论（硕）","deadline":"2026-11-09 23:59:59","status":"pending"}]}
```

## Errors and exit codes

| Exit | `error.type` | Meaning |
|---|---|---|
| 0 | — | success (`ok:true`) |
| 2 | `usage` | invalid CLI arguments |
| 3 | `auth` | missing/invalid credentials or session |
| 4 | `network` | DNS/TLS/connect/timeout |
| 5 | `parse` / `not_found` / `upstream` | upstream/JSON/parse problem |
| 6 | — (`verification.status=failed`) | submit reported success but read-only verification could not confirm it (`verified:false`); stdout stays `ok:true`, check manually and never resubmit blindly |
| 7 | — (`verification.status=error`) | submit reported success but verification could not run (`verified:null`, top-level `warning`); check manually |

Error messages and hints are redacted of known secret values. Usage errors
are JSON too (not argparse prose); `--help` remains normal text.

## Tests

Offline only (no network, no credentials):

```bash
uv run pytest
```

The tests cover timestamp parsing, filtering, attachment normalization,
`homework list --new` state, cookie round-tripping, session-file permissions and
redaction, credentials precedence, CLI error envelopes/exit codes, probe
helpers, the electricity default-dorm precedence / `buildings` subcommand
(with a stubbed electricity client, no network), and post-submit verification
(`verify_submission`, `homework submit --no-verify`, exit codes
6/7, `homework verify`).

## Known limitations (verified 2026-10-21)

- `my.bupt.edu.cn` resolves to campus-private **`10.3.55.84`** (DoH evidence).
  From typical off-campus networks HTTP times out and HTTPS resets TLS → `notices`/`card` return a
  network error. Not an API path change. WebVPN (`webvpn.bupt.edu.cn`) is
  reachable but password login is under maintenance (WeChat QR only).
- `jwgl.bupt.edu.cn` and `jwxt.bupt.edu.cn` reset TLS (`UNEXPECTED_EOF`) →
  DOM timetable/scores cannot be used.
- `jwglweixin.bupt.edu.cn` is only reachable over **HTTPS** (HTTP times out);
  the vendored SDK was patched accordingly. The mobile backend rejects the CAS
  password (`该帐号不存在或密码错误`), so `schedule/exams/scores/classrooms`
  query `yjxt.bupt.edu.cn` via `bupt schedule` instead.
- UCloud attachments live in `assignmentResource` (not `attachmentList`);
  the vendored parser was patched to surface them.
- The UCloud OAuth ticket exchange must not follow the redirect into
  `ucloud.bupt.edu.cn`; the ticket is read from the `Location` header.


## 中文文档

中文说明是默认文档：[README.md](README.md)（安装、凭据、命令表、宿舍电费、作业提交、研究生课表与日历导出，以及已知限制）。

## Attribution / license

MIT. The UCloud/CAS/portal/JWGL clients are vendored and adapted from
[Windy-Dance/campus-api](https://github.com/Windy-Dance/campus-api)
(`python/campus_api/`, MIT); see [`NOTICE`](NOTICE) and
`src/bupt_cli/_vendor/campus_api/LICENSE`. Endpoint research also referenced
ZeroCore-Zero/BUPT_APIS, YouXam/ucloud, Yokumii/bupt-api-collected,
wzekin/bupt-api and uarix/BUPT-API-Collection. All campus systems remain the
property of BUPT.
