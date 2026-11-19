# bupt — a command-line client for BUPT campus services

**English** | [简体中文](README.md) | [使用说明](使用说明.md)

An unofficial, native-Rust single-file command line client for Beijing University of Posts and Telecommunications campus services, built for humans and agents: every invocation prints one JSON document (`schema: "bupt.v1"`), and all state lives under `~/.bupt-cli/` (mode `0600`).

**MIT licensed.** The source ships no accounts, passwords, or sessions; run `bupt login` once to configure your own.

> Unofficial software, not a production system. Campus endpoints can change at any time. Follow your university's network rules and do not scrape at high frequency.

## Features

- **CAS single sign-on**: interactive login; read-only commands build service sessions lazily and never re-submit your password
- **UCloud course space**: courses, assignment lists/details, teacher-attachment downloads, submission previews and read-only verification; a real submit requires `--i-confirm-submit`
- **Graduate academics (yjxt)**: term timetable, teaching calendar, exams, and iCalendar export over **direct HTTPS — no browser**
- **Undergraduate academics (mobile JWGL)**: timetable, exams, scores, free classrooms
- **Library** (`opac.bupt.edu.cn`): anonymous title search, physical holdings, electronic copies
- **Campus portal** (`my.bupt.edu.cn`): notices, campus-card balance
- **Dorm electricity** (`app.bupt.edu.cn`): Shahe / Xitucheng buildings and balance
- **`bupt diagnose`**: a live availability matrix for public and session-bound endpoints
- **`bupt update --check` / `bupt uninstall`**: read-only update checks and a conservative uninstall

## Verified status (macOS Apple Silicon, 2026-11)

| Area | Status | Evidence |
|---|---|---|
| `login` (graduate) | ✅ live | one password POST establishes the CAS session |
| `courses` / `homework` (list, `--course`, `--new`, `show`, `download`, `verify`, `submit --dry-run`) | ✅ live | 11 courses; a real teacher attachment downloaded; a submitted assignment verified read-only |
| graduate `schedule` / `update` / `calendar` / `export` | ✅ live | 20 teaching weeks, 6 courses; 96 ICS events match the file; refresh ≈ 0.8 s |
| graduate `exams` | ✅ live | query succeeds and returns 0 exams this term |
| `library search` / `hold` / `ebook` | ✅ live | anonymous; 3 titles, 10 holdings |
| `electricity buildings --area xitucheng` | ✅ live | 12 dorm buildings |
| `diagnose` | ✅ live | 14 endpoints: 13 ok, 1 skipped, 0 failed |
| `update --check` / `uninstall --dry-run` / `logout` | ✅ live (isolated copies) | nothing installed or deleted |
| portal `notices` / `card` | ⛔ environment | the test Mac cannot reach `my.bupt.edu.cn` over HTTPS |
| `electricity --area shahe` | ⛔ upstream | the endpoint returns business error `e=1` |
| a real homework submission | ⏳ pending | wired and gated; awaiting an agreed target assignment |
| Windows x64 | ⏳ pending | see Platform support |

## Platform support

The current release is a **macOS Apple Silicon** single-file binary. Windows x64 is a stated goal (credential storage, the system `curl.exe` call, and self-replacement branches exist) but has not been validated on a real Windows machine yet.

## Install

Download `bupt-macos-arm64.zip` from [Releases](https://github.com/tututuhehehe/bupt-cli/releases):

```bash
unzip bupt-macos-arm64.zip && chmod +x bupt-macos-arm64/bupt
bupt --version        # expect: bupt 0.6.0
```

Put the binary anywhere on your `PATH`. Build it yourself (the script smoke-tests offline before packaging):

```bash
./build-rust.sh          # -> dist/bupt-macos-arm64.zip
```

The binary only shells out to the system `curl` — **no Python, no browser, and it never reads your Chrome profile**.

## Configure an account

```zsh
$ bupt login
学号 (student id)：2026xxxxxx
密码 (password, hidden)：
身份 (1=graduate 2=undergraduate)：1
```

- Credentials live only in `~/.bupt-cli/credentials.json` (`0600`). The `BUPT_USERNAME` / `BUPT_PASSWORD` / `BUPT_ROLE` environment fallbacks are **gone**; having only those in the environment is an explicit auth error.
- `role` is `graduate` or `undergraduate` and selects the academics data source.
- Treat `~/.bupt-cli/session.json` like a credential (cookies and tokens, no password).
- `bupt status` prints sanitized metadata only; `bupt logout` removes credentials and sessions but keeps the timetable snapshot and settings.

## Commands

Every command prints compact JSON to stdout (`--pretty` to indent). Errors go to stderr as `{"schema":"bupt.v1","ok":false,"error":{"type":"...","message":"...","hint":"..."}}` with layered exit codes:

| Code | Layer | Meaning |
|---|---|---|
| 0 | — | success |
| 2 | usage | invalid arguments or local config, rejected before any network use |
| 3 | auth | missing/expired credentials or session; the hint points at `bupt login` |
| 4 | network | connection failure or timeout |
| 5 | upstream | campus endpoint misbehaved or changed shape |
| 6 | — | submission outcome unknown; never auto-retried |
| 7 | — | post-submit verification failed or errored |

| Command | Purpose |
|---|---|
| `bupt login [--user ID] [--password PASS] --role graduate\|undergraduate` | register and verify the account |
| `bupt status` / `bupt logout` | sanitized local state / forget credentials and sessions |
| `bupt diagnose [--only public,cas,ucloud]` | availability matrix; submits no passwords, writes no state |
| `bupt courses` | UCloud course sites |
| `bupt homework [--undone] [--course ID] [--new DAYS] [--limit N]` | assignment list |
| `bupt homework show ID` / `download ID [--dir DIR]` | detail / teacher attachments (≤ 8 MiB each) |
| `bupt homework submit ID --dry-run` | preview the planned upload and submit; zero writes |
| `bupt homework submit ID --i-confirm-submit [--file PATH]… [--text …]` | upload and submit exactly once; ambiguous outcomes point at read-only verification |
| `bupt homework verify ID [--resource-id RID]… [--text TEXT]` | read-only verification |
| `bupt schedule [--today\|--tomorrow\|--date …\|--week N] [--raw]` | local snapshot reads |
| `bupt schedule update` / `calendar` / `export [--out PATH]` | refresh / week-to-date table / iCalendar export |
| `bupt exams` | exam schedule, routed by role; empty results never masquerade as parse failures |
| `bupt scores` / `bupt classrooms [--campus 01\|04]` | undergraduate scores / free classrooms |
| `bupt library search KEYWORD` / `hold ID` / `ebook ID` | anonymous library lookups |
| `bupt electricity --building B --room R` / `buildings [--area …]` | dorm balance / building list |
| `bupt notices [--type …] [--count N]` / `notices show URL` / `card` | portal notices and campus-card balance |
| `bupt update --check [--repo OWNER/REPO]` | read-only release check |
| `bupt uninstall [--keep-data] [--dry-run]` | conservative uninstall with guardrails |

Retired aliases: `probe`→`diagnose`, `homework list`→`homework`, `schedule exams`→`exams`, `status --check`, `login --jwgl`.

## Privacy and safety

- Credential and session files are `0600`, written via temp-file + atomic rename; symlinks and loose permissions are refused
- All HTTPS goes through the system `curl` with the request passed via stdin config — passwords, tickets, and cookies never appear in argv
- Redirects are validated hop by hop; advertised HTTP destinations are upgraded to HTTPS and never followed in the clear
- Graduate academics run browserless: no site challenge scripts are executed
- Homework submission is read-only by default; `--dry-run` performs zero writes, a confirmed run submits at most once, and unknown outcomes exit 6 pointing at `verify`
- Uninstall only deletes files the tool itself owns and reports skipped items honestly

## Known limitations

- Portal `notices`/`card` need a working campus route to `my.bupt.edu.cn`
- Shahe electricity currently answers with an upstream business error; Xitucheng works
- A real submission and the Windows build remain to be validated on real targets

## Differences from the Python version (`d7c35ef`)

- One native binary replaces the PyInstaller bundle; no Python runtime
- Graduate academics dropped headless Chrome/CDP for direct HTTPS (≈ 4× faster, zero browser dependency)
- Credentials come only from `credentials.json`; environment-variable fallbacks are removed
- `homework` lists by default, `exams` routes by role, `probe` became `diagnose` and never submits passwords
- Version jumped from 0.5.x to **0.6.0**; the full Python implementation stays in git history at `d7c35ef`

## Credits

Endpoint knowledge builds on community collections and upstream projects ([campus-api](https://github.com/Windy-Dance/campus-api), [BUPT-API-Collection](https://github.com/uarix/BUPT-API-Collection), [bupt-api](https://github.com/wzekin/bupt-api), [bupt-api-collected](https://github.com/Yokumii/bupt-api-collected)); every endpoint was re-validated during the port. See [LICENSE](LICENSE).
