---
name: bupt-cli
description: >-
  Use this for anything on BUPT (北邮) campus systems: UCloud homework
  (list/new/show/download/submit), courses, dorm electricity balance,
  timetable/exams/scores.
---
# bupt CLI

After install, run `bupt <command>`. Output is JSON; add `--pretty` for indented output. Once credentials are configured, login is cached and reused automatically. Run `bupt <command> --help` for the full flags.

## Common commands
- `bupt homework list [--undone] [--course ID] [--new DAYS]`: list assignments.
- `bupt homework show ID`: an assignment's details, attachments, and submission state (`submitted`, `submitted_at`, submitted attachments).
- `bupt homework download ID --dir DIR`: download the teacher's attachments.
- `bupt homework verify ID [--resource-id RID ...]`: read-only check that a submission is really there.
- `bupt courses`: this semester's courses.
- `bupt electricity`: dorm electricity balance. Configure your own dorm with `--building/--room`, `BUPT_DORM_*`, or `~/.bupt-cli/config.json` first; the built-in value is only a placeholder. The output has `low_balance`.
- `bupt status` / `bupt probe`: check the session and which endpoints work.
- `schedule/exams/scores/classrooms` are experimental (they need a separate 教务 password). `notices/card` depend on the campus-only portal and usually fail off-campus.

## Submitting homework (only with the user's explicit consent)
1. `bupt homework submit ID --file f.pdf --text "..." --dry-run`: preview first.
2. Show the user the content you'll submit. Do a real submit only after they explicitly agree: `bupt homework submit ID --file f.pdf --i-confirm-submit`.
3. Every submit sends the full set of content and attachments, overwriting the previous submission, so include every file each time.
4. A real submit verifies itself afterwards (`verified`, `submitted_at`). Exit 6 means the submission couldn't be confirmed, and exit 7 means the check itself failed. In either case check with `show`/`verify` and tell the user; never resubmit on your own.

## Notes
- Don't log in repeatedly in quick succession; the account can get locked.
- Never print credentials.
