# 作业与课程迁移测试（homework lane）

对照 Python：`d7c35ef`。本 lane 负责 `courses` 与 `homework`（默认列表 / `show` / `download` / `submit` / `verify`），对应 Python 测试：

- `tests/test_courses.py`
- `tests/test_homework_new.py`
- `tests/test_homework_show.py`
- `tests/test_verify.py`

测试分三层：

- `tests/contract_homework.rs`：当前**可运行**的公开进程 CLI 契约用例，只使用共享 `Sandbox`（隔离家目录/配置/缓存）。覆盖解析器、无凭据鉴权边界、参数门禁，不访问上游。
- `src/homework/submit.rs` 的 `#[cfg(test)]` 单元用例：当前**可运行**的进程内缝，使用共享 `transport::testing::Scripted`、合成 session 和临时文件。覆盖提交门禁、零次/一次/未知 upload/submit 结果和确认回执分类；不读取真实凭据，不回退到网络。命令规格与本地校验保留在较小的 `src/homework.rs`。
- `tests/homework_replay/cases.rs`：**草稿，未被 Cargo 自动发现，也尚不可运行**。它保留完整合成录制驱动的业务用例，等待库内部的注入式 transport/clock/session 测试缝接入。没有运行时环境变量回放，没有真实网络/凭据/浏览器。

## 有意变化

- `bupt homework` 是唯一列表入口；旧别名 `bupt homework list` 不再存在。`homework` 仍接受 `--undone` / `--course` / `--new` / `--limit`。
- 所有非交互输出单个 JSON 文档，带 `schema: "bupt.v1"`；错误走 stderr。成功结果的 `ok` 字段保留。
- `--pretty` 只改变排版。
- 提交安全：没有 `--i-confirm-submit` 不写；`--dry-run` 不写。可能已提交但无法确认时先只读核验，绝不自动重发 submit。

## 可运行本地用例

`tests/contract_homework.rs` 只断言本地可观察行为：

| 用例 | 覆盖点 |
|---|---|
| `courses_reaches_the_auth_boundary_without_credentials` | `courses` 接受无参数形式并进入凭据层（auth，退出 3） |
| `environment_only_credentials_are_ignored` | 只有合成 `BUPT_USERNAME` / `BUPT_PASSWORD` / `BUPT_ROLE`、没有 `credentials.json` 时仍为 auth 3；账号环境变量不构成登录 |
| `malformed_saved_credentials_are_an_auth_error` | 损坏的 `credentials.json` 报 auth 3，提示指向登录，不回显文件内容 |
| `saved_credentials_do_not_require_a_role` | 有效 JSON 只有 username + password、没有 role 时仍能进入会话接线阶段（upstream 5）；homework 不要求 role |
| `homework_defaults_to_the_list_command` | 无凭据时 `homework` 已进入会话层（auth，退出 3），不是“需要子命令” |
| `explicit_homework_list_alias_is_rejected_as_usage` | `homework list` 在凭据/网络前退 2 |
| `default_homework_accepts_the_list_flags` | 默认列表接受 `--undone` / `--limit` / `--course` / `--new`（继续到 auth 层） |
| `invalid_list_values_are_usage_errors_before_credentials` | 负 `--limit` / `--new` 退 2 |
| `homework_subcommands_accept_documented_flags_before_credentials` | `show` / `download` / `verify` / `submit` 的文档参数均可解析并继续到凭据层 |
| `show_rejects_a_non_numeric_course_id_before_credentials` | 非数字 `--course` 退 2 |
| `submit_rejects_an_empty_payload_before_credentials` | 无文件/正文/资源 id 退 2 |
| `submit_rejects_dry_run_with_confirmation_before_credentials` | `--dry-run` 与 `--i-confirm-submit` 互斥，退 2 |
| `submit_without_confirmation_is_usage_before_credentials` | 有文件/正文但缺少 `--i-confirm-submit` 时在凭据与网络前退 2，错误点名确认 flag |
| `submit_rejects_a_missing_file_before_credentials` | 缺失文件在凭据/网络前退 2 |

这些用例共用真实进程 `Sandbox`，不安装任何假上游，也无法断言业务数据成功路径。

### 源内单元用例（`src/homework/submit.rs`）

这些用例直接调用提交 helper，只替换 transport 与 session，不启动第二个进程：

| 用例 | 覆盖点 |
|---|---|
| `submit_without_confirmation_never_contacts_transport` | 未确认时退 2，`transport.calls` 为空；门禁先于任何读取或写入 |
| `submit_dry_run_reads_the_plan_without_writes` | 只发 detail / submit-view 两个 GET；计划含已有附件；无 POST、无 verification |
| `confirmed_submit_without_verification_uploads_then_submits_once` | 写入序列恰为 `[upload, submit]`；submit 正文/附件正确；`--no-verify` 返回 `verification.status=skipped` |
| `explicit_success_flag_or_code_alone_is_accepted` | 只有明确 `success: true` 或 `code=200` 时才算肯定回执，不把缺省值当成功 |
| `empty_submit_acknowledgement_is_ambiguous_and_verify_only` | 2xx `{}` 退 6，`submitted=null`，warning/diagnostic 只指向 `verify`；写入恰为 `[upload, submit]` |
| `invalid_submit_acknowledgement_is_ambiguous_and_verify_only` | 非法 `success` 标志、互相矛盾的 success/code 等畸形回执退 6，不报 `submitted=true`；不重发 submit |
| `non_json_submit_response_is_ambiguous_and_verify_only` | 2xx 非 JSON 响应退 6，`submitted=null`，verify-only 指引；不重发 submit |
| `timeout_server_error_and_redirect_statuses_after_submit_are_ambiguous_and_not_retried` | 302 / 408 / 502 退 6，`submitted=null`，verify-only 指引；恰一次 submit，不重发 |
| `explicit_http_and_upstream_refusals_are_not_ambiguous_and_not_retried` | 明确 400 与 JSON 负回执走错误（HTTP/上游 5，401/403 鉴权 3），不回显原始 `msg`/token，不重发 |
| `lost_submit_response_is_ambiguous_and_never_retried` | 恰好一次 submit；超时/断连退 6，`submitted`/`verified` 为 null，verify-only 指引；无自动重发 |
| `upstream_message_echoing_token_is_redacted_at_the_boundary` | 上游 `msg` 回显合成 token 时，结果边界统一替换为 `[REDACTED]` |

回执判定只接受肯定证据：显式 `success: true`，或认可的 `code`（0/200）。空对象、缺失/非法指示、互相矛盾的 success/code、2xx 非 JSON、408、5xx、意外重定向以及没有确认响应的 transport failure 都退 6 且 `submitted=null`，并只指向只读 `verify`。明确 4xx（408 除外）、JSON 负回执和 401/403 仍走显式错误（HTTP/上游 5，鉴权 3），不伪装成 unknown。所有分支都不自动重发 submit；transport failure 只说明没有收到确认响应，不回显原始诊断。

当前真实 CLI 在提交 helper 之前没有会话适配器，因此带凭据时仍以“session wiring 未实现”退 5，绝不发起 HTTP。凭据只从 `credentials.json` 读取（username + 非空密码），不读取账号环境变量，也不要求 role；`courses`、作业列表、`show`、`download`、`verify` 同样在通过凭据检查后以 unsupported 退 5。本批没有课程分页、列表、详情、下载或只读核验实现，`verify` 保持 pending。成功提交但未跑 `--no-verify` 时 helper 也返回 6 且 `verification.status=not_run`，明确表示未核验，不伪造 `verified=true`。

结果里不序列化 detail 原始对象或 session 结构；接受的 `upstream.msg` 和错误文本在输出边界统一脱敏，`Session` 不实现 `Debug`。新增 fixture `homework_submit_echo_token.json` 让上游回显合成 token，供后续进程级 seam 复用；当前对应边界测试已在源内通过。

### Scaffold 边界

`src/homework/submit.rs` 的 multipart renderer 和固定 boundary 只用于隔离的 transport journal 测试。`upload_file` 目前整文件读入内存；在连接真实 transport 前必须替换为经过审查的 multipart encoder，并加上有界/流式读取。本批没有连接它，真实 CLI 仍停在 session wiring。

## 草稿：合成录制与注入式测试缝

`tests/homework_replay/cases.rs` 不是自动测试入口。它当前调用：

```rust
run_scenario(&sandbox, &fixture_path, args) -> Run { invocation, writes }
```

该函数以 `unimplemented!` 明确标记测试缝尚未接入；因此草稿在被手动纳入编译前不会运行，也不会伪装通过。接入时应由 Rust 库提供内部测试适配器，满足：

- 从 `tests/fixtures/contracts/homework/*.json` 读取一个合成录制；
- 在**进程内部**替换 transport、固定 clock、提供合成 session；
- 不读取环境变量覆盖、真实凭据、浏览器会话，不回退到网络；
- 返回捕获的 CLI 输出，以及所有**尝试过的** upload/submit 写入日志；
- 写入日志必须在匹配响应**之前**记录，包含未匹配 exchange 的请求。测试断言精确顺序和次数，不能因为“未匹配请求报错”就推断没有写入；
- 录制中的 `response` 可以是 `status + json/body`，也可以用 `transport_error` 表示请求已发出但响应丢失（用于 lost-response 用例）；
- 未匹配的非写入请求应让命令失败且不再访问真实网络；未消费的录制允许存在。

录制只含合成账号、合成 token、`.invalid` 主机和合成正文。不是运行时回放开关。

## 草稿覆盖矩阵

以下用例名都指 `tests/homework_replay/cases.rs` 中的草稿，**尚未运行**。其中提交门禁、dry-run、确认一次写入、响应丢失四类已有上面源内单元用例覆盖 helper 层；草稿仍保留为后续完整进程级录制验收。

### courses

| Python 用例 | 草稿用例 | 覆盖点 |
|---|---|---|
| `test_courses_concatenates_pages`、`test_courses_deduplicates_across_pages` | `courses_concatenates_pages_deduplicates_and_stops_on_a_short_page` | 跨页拼接、页序、site id 去重、短页终止 |
| `test_courses_keeps_every_empty_site_id` | `courses_keeps_every_empty_site_id` | 空 `siteId` 不被去重吞掉 |
| `test_courses_stops_on_a_short_page` | `courses_stops_on_a_short_page_without_a_second_request` | 短页后不再请求 |
| `test_courses_stops_on_an_empty_page`、`test_courses_returns_empty_without_error` | `courses_stops_on_an_empty_page_without_a_second_request` | 空页结束，空结果不是错误 |
| `test_courses_respects_the_page_limit` | `courses_stops_at_the_safety_page_limit` | 10 页上限 |

### homework 列表 / 过滤 / `--new`

| Python 用例 | 草稿用例 | 覆盖点 |
|---|---|---|
| `test_list_without_new_never_calls_the_detail_endpoint` | `default_homework_list_sorts_by_deadline_and_exposes_missing_metadata` | 默认列表、截止时间排序、缺失字段为 null、不查详情 |
| `test_new_keeps_only_recent_assignments`、`test_new_sorts_newest_publication_first`、`test_new_uses_list_publish_time_without_a_detail_call` | `new_filter_uses_list_publish_times_and_sorts_newest_first` | 列表自带发布时间、窗口过滤、发布时间倒序、零详情请求 |
| `test_new_discloses_assignments_it_could_not_date`、`test_new_reports_unknown_count_when_lookup_budget_runs_out` | `new_filter_reports_undated_assignments_and_respects_the_lookup_budget` | 30 次预算、`unknown` / `unknown_publish_time` / `lookup_limit` 结构化披露 |
| `test_new_looks_up_only_the_filtered_assignments` | `new_filter_looks_up_only_assignments_that_pass_the_filters` | 先过 `--course`/`--undone` 再花预算 |
| `test_new_reports_detail_failures`、`test_failed_lookups_do_not_consume_the_budget` | `new_filter_failed_lookups_do_not_consume_the_budget` | 失败计入 `failed`、不消耗预算、后续仍解析 |
| `test_new_with_empty_list_says_so`、`test_new_says_so_when_nothing_falls_in_the_window` | `new_filter_explains_an_empty_list_and_an_empty_window` | 空上游与空窗口的 note 必须非空且彼此可区分；不锁定旧文案 |
| `test_auth_failure_is_raised_not_reported_as_missing_publish_time` | `new_filter_auth_failure_is_not_reported_as_missing_publish_time` | 401 退 3 / `error.type=auth`，不伪装成 unknown |
| 过滤/排序回归 | `undone_filter_uses_the_undone_endpoint_and_sorts_by_deadline` | `--undone` 数据源与截止时间排序 |
| 组合过滤回归 | `course_and_undone_filters_combine` | `--course` 优先取课程作业，再应用 `--undone` |

### 详情 / 下载

| Python 用例 | 草稿用例 | 覆盖点 |
|---|---|---|
| `test_show_submitted_uses_submit_view_and_matching_site`、`test_homework_show_reports_status` | `show_merges_detail_submit_view_and_student_list` | 详情、submit-view、同名课程站点、学生列表 `submitTime`、附件、正文、评语、状态 |
| `test_show_not_submitted_skips_list_lookup`、`test_get_detail_assignment_status_99_means_not_submitted` | `show_not_submitted_does_not_fabricate_submission_fields` | 未交不伪造正文/附件/提交时间，不查学生列表 |
| `test_show_succeeds_when_submit_view_raises` | `show_survives_submit_view_failure_and_redacts_the_error` | submit-view 失败仍保留详情，错误非空、有界、脱敏 |
| 下载回归 | `download_saves_attachments_and_skips_entries_without_urls` | 预览 URL、保存附件、无 URL 计入 skipped、零写入 |
| 下载回归 | `download_does_not_overwrite_without_force` | 已存在默认跳过，`--force` 才覆盖，零写入 |

### 提交门禁 / 校验输出边界

所有提交用例都断言 `Run.writes` 的精确 upload/submit 序列，而不是从 exchange 是否匹配推断写入。

| Python 用例 | 草稿用例 | 覆盖点 |
|---|---|---|
| `test_submit_without_confirm_uploads_nothing` | `submit_without_confirmation_exits_two_before_upload` | 退 2、点名确认 flag、write journal 为空 |
| `test_dry_run_uploads_and_submits_nothing` | `submit_dry_run_never_uploads_or_submits` | `dry_run=true`、计划字段、无 verification、write journal 为空 |
| `test_submit_with_confirm_uploads_then_submits`、`test_submit_uploaded_file_is_the_expected_attachment_set`、`test_submit_verified_pass_exits_zero` | `submit_confirmed_uploads_submits_once_and_verifies` | 写入序列恰为 `[upload, submit]`、退出 0、`verified=true` |
| `test_submit_no_verify_skips_all_readonly_checks` | `submit_no_verify_skips_read_only_checks` | 写入序列恰为 `[upload, submit]`、`verification={status:skipped}`、不请求提交后核验 |
| `test_submit_unverified_mismatch_exits_six` | `submit_unverified_exits_six_without_resubmitting` | 恰一次 submit、退出 6、`verified=false`、失败检查结构化、提示指向只读检查动作（show/verify） |
| `test_submit_verification_error_exits_seven` | `submit_verification_error_exits_seven_without_resubmitting` | 恰一次 submit、退出 7、`verified=null`、`status=error`、warning 非空、提示指向只读检查动作（show/verify） |
| 新增 pending：submit 响应丢失 | `submit_lost_response_is_ambiguous_without_automatic_retry` | 恰好一次 submit、不自动重试、非零退出、不得报 `verified=true`、提示指向 verify 动作 |
| `test_verify_subcommand_pass`、`test_verify_without_resource_id_accepts_visible_submission` | `verify_bare_accepts_a_visible_submission_and_normalises_text` | 只读 verify、无 `--resource-id` 只要求可见提交、正文归一化、零写入 |
| `test_mismatch_on_both_attempts_is_failed`、`test_content_mismatch_when_text_expected`、`test_verify_subcommand_failure_exits_six`、`test_verify_with_resource_id_still_compares_exactly` | `verify_resource_mismatch_exits_six_with_the_read_only_hint` | 附件/正文严格比对、退出 6、零写入、提示指向只读检查动作（show/verify） |
| `test_network_error_on_last_attempt_is_error`、`test_error_message_is_redacted_and_bounded` | `verify_error_exits_seven_without_submitting` | 末次错误退 7、warning 非空、错误有界且脱敏、零写入 |

警告与 note 的断言只依赖结构化字段（`verified`、`verification.status`、`failed_checks`、`unknown`、`unknown_publish_time` 等）以及可执行的只读检查动作（文本中出现 `show` 或 `verify`；lost-response 用例特指 `verify`）。不锁定旧英文/中文提示原文。

## 未覆盖 / 延期

- `test_courses.py` 的 `test_fresh_cache_is_used_without_a_fetch`、`test_stale_cache_triggers_a_fetch`、`test_missing_cache_triggers_a_fetch` 属于课表/教务 lane。
- Python vendor 的同步/异步私有解析用例（字符串 `"0"`、`submitInfo.isSubmit`、async）不迁移私有函数；命令级只覆盖 `assignmentStatus=0/99` 的可观察结果。
- `show` 的空 submit-view 回退、学生列表查询失败时 `submitted_at=null`、`--course` 限制站点查找：尚无单独录制。
- `preview_filename` 的 RFC 5987 `filename*=UTF-8''`、引号、文件名内分号等变体。
- verify 的“第二次尝试才通过”、无 `--text` 时忽略正文、空正文跳过正文检查、sentinel `assignmentId=-1` 且无附件、非网络意外异常、expected/observed 文本 200 字截断。
- verify 草稿仍带公开 CLI 不存在的 `--course`；接线前要按详情目标课程重做参数与录制，不能为了草稿直接新增该参数。
- 固定 5 秒重试等待未做墙钟断言；草稿只保留 `attempts` 与退出码边界。
- 上传失败、submit-view 401 后的续期、真实校园网上游验收未运行。真实提交/上传不在本阶段执行，校园网验收需用户单独确认。

## 下一批接线顺序

1. 由 identity lane 提供真实 session 适配器，替换 `homework::submit::load_session` 的 unsupported 退出；提交 helper 已通过 `Context.transport` 注入，接线时不得新增环境变量回放。
2. 先把源内提交用例接到 fixture/进程级 seam（含 `homework_submit_echo_token.json`），再为 empty/invalid/refusal 回执补录制，并补 `submit_confirmed_uploads_submits_once_and_verifies`、`submit_unverified_exits_six_without_resubmitting`、`submit_verification_error_exits_seven_without_resubmitting`。
3. 按 `courses` 分页、列表/`--new`、详情/下载、只读 `verify` 的顺序接线；每类先实现只读请求，再引入写请求。
4. 校园网真实提交/上传与双平台验收单独执行，需用户确认。

## 当前验证状态

- `cargo fmt --check`：通过。
- `cargo test --locked --offline --lib`：13 个通过（2 个共享 CLI + 11 个 homework 提交 helper）。
- `cargo test --locked --offline --test contract_homework`：11 个通过。
- `tests/homework_replay/cases.rs`：不在 Cargo 自动测试集合中；`run_scenario` 未接入，因此 30 个草稿全部不可运行，也没有伪装通过。
- 同一 worktree 的 `contract_lifecycle` 红跑仍有 12 个失败，均属于 `status` / `update` / `uninstall` 等其它 lane 的空实现；`homework` 入口、帮助树、移除别名相关用例已通过。该红跑不是本 lane 的验收门禁。
- 未运行真实校园网、浏览器会话、上传或提交；没有新增环境变量回放开关。
