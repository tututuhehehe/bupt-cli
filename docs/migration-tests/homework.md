# 作业与课程迁移测试（homework lane）

对照 Python：`d7c35ef`。本 lane 负责 `courses` 与 `homework`（默认列表 / `show` / `download` / `submit` / `verify`），对应 Python 测试：

- `tests/test_courses.py`
- `tests/test_homework_new.py`
- `tests/test_homework_show.py`
- `tests/test_verify.py`

测试分三层（源内单元层按 read / detail / show / verify / submit / text 拆分）：

- `tests/contract_homework.rs`：当前**可运行**的公开进程 CLI 契约用例，只使用共享 `Sandbox`（隔离家目录/配置/缓存）。覆盖解析器、无凭据鉴权边界、参数门禁，不访问上游。
- `src/homework/read.rs` 的 `#[cfg(test)]` 单元用例：当前**可运行**的只读进程内缝，使用共享 `transport::testing::Scripted` 和合成 session。覆盖课程分页/去重/终止、默认 all 与 `--course` 作业分页、作业字段映射/排序/过滤/`--limit`、undone 映射、空 undone、未授权/畸形响应和缺 `user_id`；不读取真实凭据，不回退到网络。公开 `courses` / 默认列表 / `show` / `verify` 已改用真实 `identity::require_ucloud_session` 接线。
- `src/homework/detail.rs` 的 `#[cfg(test)]` 单元用例：共享的详情 / submit-view / 学生列表 / 附件 / preview filename 解析。`show`、`verify` 与 `submit` 的提交前读取都走这一层，避免三套字段和错误语义漂移。
- `src/homework/show.rs` 的 `#[cfg(test)]` 单元用例：组装 `homework show` 的详情、submit-view、附件 URL、学生列表提交时间和状态；所有失败路径都证明零 upload/submit。
- `src/homework/verify.rs` 的 `#[cfg(test)]` 单元用例：纯校验算法（注入 `VerifyReader`、sleep）与命令级只读路径；覆盖附件集合、正文归一化、失败 / 错误退出语义和零写入。
- `src/homework/submit.rs` 的 `#[cfg(test)]` 单元用例：当前**可运行**的进程内缝，使用共享 `transport::testing::Scripted`、合成 session 和临时文件。覆盖提交门禁、零次/一次/未知 upload/submit 结果和确认回执分类；不读取真实凭据，不回退到网络。命令规格与本地校验保留在较小的 `src/homework.rs`。
- `src/homework/text.rs` 的 `#[cfg(test)]` 单元用例：HTML 去标签/实体解码、空白归一化、先脱敏后截断。
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
| `legacy_roleless_credentials_cannot_prove_a_verified_login` | 只有 username + password、没有 role 的旧凭据不能打开已验证 session（auth 3），且不创建 `cache/session.json` |
| `read_commands_reach_the_verified_session_boundary` | 同一份 roleless 凭据下，`courses`、默认 `homework`、`--undone`、`--course`、`--limit`、`--new`、`show`、`verify` 都进入已验证 session 边界（auth 3）；其中 `show` / `verify` 已不再是 unsupported |
| `deferred_new_filter_stops_before_network` | 有 role 的合成凭据 + `--new 7` 仍退 upstream 5，不创建 session，证明 `--new` 延期而非静默降级 |
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

### 源内只读单元用例（`src/homework/read.rs`，synthetic slice）

这些用例直接调用只读 helper，只替换 transport 与 session，不启动第二个进程。本切片只用合成 `Scripted` transport 和合成 session，不接真实 transport/session，也不是运行时回放。

| 用例 | 覆盖点 |
|---|---|
| `courses_first_page_keeps_all_eleven_unique_site_ids` | 合成首屏 11 行、site id 全部存在且唯一；短页后只请求一次 |
| `course_mapping_does_not_replace_explicit_zero_or_false_with_fallback_keys` | 显式 `siteId=0` / `siteName=false` 先命中 `_first_present`，再按 `or ""` 变成空，不错误回退到 `id` / `className`；空字符串仍回退 |
| `courses_short_page_stops_without_a_second_request` | 2 行短页后立即停止，不猜测还有下一页 |
| `courses_empty_page_stops_without_a_second_request` | 空页返回 `count=0` 和空数组，不报错、不请求第二页 |
| `courses_concatenate_pages_and_deduplicate_repeated_ids` | 跨页保持页序，只去掉重复的非空 id；重复项不覆盖首条元数据 |
| `courses_keep_every_empty_site_id` | 空 `siteId` 无法去重，全部保留 |
| `courses_stop_when_a_full_page_adds_no_new_ids` | 整页都是重复 id 时停止，不无限翻页 |
| `courses_stop_at_the_ten_page_safety_limit` | 10 页安全上限，最多 500 行，不请求第 11 页 |
| `unauthorized_courses_and_undone_stop_after_one_request` | 401/403 都退鉴权 3，courses 和 undone 各只请求一次，不重试 |
| `http_503_is_upstream_5_without_retry` | 实际 HTTP 503 退 upstream 5，只读一次，不重试，不回显原始 `msg` / token |
| `transport_error_remains_network_4_without_retry` | `transport.send` 返回 network 类错误时保持 4，只读一次，不重试 |
| `unexpected_course_payloads_are_parse_errors_without_retry` | 缺 `data`、非对象、坏 records、坏 total 都退 parse 5，只请求一次 |
| `api_envelope_error_remains_network_4_without_retry` | JSON API envelope 的 `code` 非 0/200 仍按 Python `_request` 基线退网络 4，只请求一次，不回显原始 `msg` |
| `undone_mapping_matches_the_python_fields` | 现有 undone fixture 的 activity/site/begin/end 映射、计数和 `submitted=false` |
| `undone_mapping_keeps_python_fallback_rules_without_course_enrichment` | `id`/`assignmentTitle` 回退、`siteId=0` 视为缺失、`-1` 原样保留；不伪造课程或截止时间 |
| `empty_undone_is_success_and_not_an_auth_error` | 空 `undoneList` 是成功结果，`siteNum=0`、`undoneNum=0`，不是鉴权失败 |
| `unexpected_undone_payloads_are_parse_errors_without_retry` | 非对象 data、缺 `undoneList`、非对象记录都退 parse 5，只请求一次 |
| `undone_without_user_id_fails_before_transport` | 合成 session 缺 `user_id` 时退鉴权 3，transport 零调用 |
| `homework_all_paginates_course_and_student_pages` | 默认 all：课程页按 `size=30` 取站点，某课程第一页 50 行、第二页 10 行时继续翻页；POST body 带 `siteId/userId/current/size`，课程名回填缺失 `siteName` |
| `homework_course_list_paginates_until_total` | `--course`：第一页 50 行 + 第二页 5 行、`total=55` 时取全 55 行，不再只取第一页 |
| `homework_course_list_short_page_stops_without_a_second_request` | `--course` 短页立即停止，不猜测下一页 |
| `homework_course_list_repeated_page_is_a_parse_error` | 同一课程页内容重复时退 parse 5，不无限翻页 |
| `homework_course_walk_paginates_by_total_and_stops_at_thirty_sites` | 默认 all 的课程列表按 `total` 继续一页，并在 30 个站点上限截断，不请求第 3 页 |
| `homework_course_walk_repeated_page_is_a_parse_error` | 默认 all 的课程页重复时退 parse 5，不无限翻页 |
| `homework_all_skips_courses_without_a_site_id` | 空 `siteId` 的课程不请求学生列表；有效课程照常取作业 |
| `homework_page_mapping_matches_python_status_and_fallback_rules` | `assignmentStatus=0` / `"SUBMITTED"` / 非空 `submitTime` 算已交；boolean 状态不算；空 `activityId` 回退 `id`；空 `beginTime`/`endTime` 不伪造 |
| `homework_list_output_sorts_filters_and_limits_like_python` | `assignment_dict` 字段、按截止时间升序（无截止最后）、`--course` / `--undone` 过滤和 `--limit` 截断；`hours_remaining`、`overdue`、三态 `status` 由注入时钟固定 |
| `homework_list_deduplicates_nonempty_ids_and_keeps_empty_ids` | 重复非空 `activityId` 只保留首条；空 id 全部保留 |
| `homework_list_without_user_id_fails_before_transport` | 默认 all 和 `--course` 在缺 `user_id` 时退 auth 3，transport 零调用 |
| `homework_list_auth_and_malformed_pages_fail_without_retry` | 学生列表 401 退 auth 3 且只请求一次；缺 records、坏 records、坏 total 退 parse 5 |
| `undone_cli_output_sorts_deduplicates_and_preserves_null_values` | undone 输出排序、去重、缺失 `hours_remaining` 为 null、overdue/pending 三态 |

与 Python `cmd_courses` 对齐的分页规则：`size=50`、最多 10 页、按页序拼接、只对非空 `siteId` 去重、空页/短页/整页无新 id 时停止。`get_courses` 本身不按 `total` 终止，但 Python 解析器会拒绝畸形 total，这里同样校验。课程 `siteId` / `siteName` 按 Python `_first_present(record, ...) or ""` 映射：显式 `0`/`false` 是 present，随后变成空值，**不能**被通用的 `id` / `className` 回退替换；只有 `null` 或空字符串继续尝试下一个键。

### 默认 all / `--course` 作业列表

默认 `homework` 对应 Python `_load_assignments` 的 `all` 分支：
- 课程列表按 `size=30`（`min(site_limit=30, page_size=50)`）请求，最多保留 30 个站点；`total` 表示还有行时继续下一页，重复页退 parse 5，1000 页安全上限。
- 对每个非空 `siteId` 逐课程 POST `/ykt-site/work/student/list`，body 为 `{siteId,userId,current,size:50}`；按 `total`/短页/空页停止，重复页退 parse 5。行内缺少 `siteName` 时用课程名回填。
- `--course ID` 只走该课程的学生列表；本实现会继续翻页直到 `total`/短页，这是相对 Python `get_course_homework` 只取第一页的有意补全。`--course` 仍优先于 `--undone`（与 Python `_load_assignments` 一致），随后再应用 `--undone` 过滤。
- 列表先按非空 `activityId` 去重（空 id 全部保留），再按 `--course` / `--undone` 过滤，按截止时间升序（无截止时间排最后，再按标题），最后应用 `--limit`（0 = 全部）。
- 输出沿用 Python `assignment_dict`：`id/title/course/course_id/published_at/deadline/begin_time/status/submitted/submitted_at/score/hours_remaining/overdue`，并带 `count/method/filters`。`published_at` 先查列表行的 `assignmentBeginTime`、`beginTime`、`publishTime` 等键，再回退 `begin_time`；取不到就是 null。
- `method` 为 `all` / `course` / `undone`；`--course --undone` 的 `method` 仍是 `course`。
- `--new DAYS` 本批**未接线**：正常列表已能输出列表行自带的 `published_at`，但详情补查、30 次预算、`failed`/`unknown`/`unknown_publish_time`、note、按发布时间倒序和 `truncated` 都尚未实现；当前在本地凭据检查后直接退 upstream 5，不发起网络请求。

错误分层按 Python `errors.from_campus_error` 的公开行为切分：`transport.send` 的 network failure 保持 4；实际 HTTP 401/403 退鉴权 3；实际 HTTP 4xx/5xx（含 503）退 upstream 5；两者都只请求一次、不重试。JSON API envelope 的 `code` 非 0/200 走另一条基线（`_request` 抛 `CampusNetworkError`），仍退网络 4，不因 envelope code 是 5xx 就升级为 upstream 5。

与 Python `get_undone` 对齐的映射规则：GET `/ykt-site/site/student/undone?userId=...`，`activityId`/`id` 与 `activityName`/`assignmentTitle` 依次回退，`siteId=0` 视为缺失，`is_submitted` 固定 false，`siteNum` / `undoneNum` 默认值分别为 0 / 列表长度。undone 的回退语义保持不变。**课程富化暂不实现**：真实非空 undone 可能出现 `siteId=-1` 和空 `siteName`，Python 的 `_enrich_undone_courses` 需要再扫全部课程作业并按 activity id 回填；本批没有真实非空 undone 证据，也没有接上 `get_all_homework`，因此明确延期，不声称 parity，`-1` 与空名称保持原样。

Python 的 UCloud `_request` 在 401 时会 invalidate 并重试一次；Rust 这里没有身份 lane 的续期适配器，因此只请求一次并退鉴权 3。重试/续期必须等真实 session 适配器接入后单独验证；当前“不循环”的行为已由测试固定。

### `show` / `verify` 的只读语义

`show` 的顺序与 Python `cmd_homework_show` 一致：详情 GET → 教师附件 preview URL → submit-view（best effort）→ 学生列表（仅在提交可见时）。详情失败按实际层传播（HTTP 401/403 → 3，transport failure → 4，HTTP 4xx/5xx → 5）；submit-view、preview 或学生列表失败只降级对应字段/`submitted_at`，不拖垮命令。submit-view 为空对象时保留详情派生的答案，并给出 `submit-view returned no data`；错误文本先脱敏再截到 200 字符。所有路径都不发 upload/submit。

`verify` 先用详情做 best-effort 课程名缩窄（失败则扫所有站点），然后每轮读 submit-view 和学生列表；默认两轮，第二轮前等待 5 秒（测试注入 0 秒 sleep）。附件集合只在传入 `--resource-id` 时严格比较；正文只在 `--text` 非空白时比较，并先做 HTML 去标签 + 空白归一化。最终状态取最后一轮：有 `failed` → 退出 6，只有 `error` → 退出 7，全通过 → 退出 0。submit-view 的 HTTP 401/403 在这里是校验 `error`（退出 7），不是命令级 auth 3；只有详情请求的 401/403 会被缩窄逻辑吞掉。`verified`、`submitted_at`、`verification.checks` 字段与 Python 保持同名；错误文本先脱敏再截断。所有路径都不发 upload/submit。

`show`、`verify` 与 `submit` 的提交前 detail / submit-view 读取共用 `src/homework/detail.rs`；只有 `submit` 的 upload / submit 写入和回执分类留在 `submit.rs`。
公开 CLI 的 `show::run` / `verify::run` 现在调用 `homework::load_read_session`，后者走 `identity::require_ucloud_session` 的已验证原生 session，不再返回 “session wiring 未实现”。但 `cli.rs::uses_campus_transport` 和 `transport/cas_wire.rs` 的 allowlist 仍由父会话/整合者维护；在这两处放开之前，正式二进制仍会给这些命令 `Offline`，所以本批的成功路径证据仍是进程内 helper 测试，真实校园网验收未运行。

`show`、`verify` 与 `submit` 的提交前 detail / submit-view 读取共用 `src/homework/detail.rs`；只有 `submit` 的 upload / submit 写入和回执分类留在 `submit.rs`。

### 源内单元用例（`src/homework/detail.rs`）

共享读取层只接收合成 `Scripted` / 合成 session 或直接解析 JSON；这些测试不启动 CLI，也不访问网络。

| 用例 | 覆盖点 |
|---|---|
| `detail_mapping_missing_status_is_not_submitted_and_missing_attachments_stay_empty` | 缺 `assignmentStatus` / 附件时 `submitted=false`、附件为空，不伪造提交状态 |
| `detail_status_zero_and_string_zero_are_submitted_but_false_is_not` | 详情只把 `0` / `"0"` 当已交；`false`、`99`、`"99"`、`"SUBMITTED"`、`null` 都不是已交 |
| `list_status_accepts_only_the_documented_submitted_values` | 学生列表接受 `0` / `"0"` / `"SUBMITTED"` 或非空 `submitTime`，其他值不伪造已交 |
| `submit_view_sentinels_and_attachment_lists_are_not_fabricated` | `assignmentId=-1` / `"-1"` / `null` / 空串且无附件时没有提交；数值 `0` 或非空附件表示可见提交 |
| `preview_filename_handles_encoded_and_quoted_dispositions` | `response-content-disposition` 的百分号编码、`filename*` RFC 5987 变体和缺失参数 |
| `attachment_normalization_keeps_explicit_values_and_placeholders` | 附件标题/URL/resource id/size 映射；空对象使用 `attachment-N` 占位，不发明 URL |

### 源内单元用例（`src/homework/show.rs`）

| 用例 | 覆盖点 |
|---|---|
| `submitted_show_merges_detail_submit_view_and_student_list` | 详情、教师附件 preview URL、submit-view 正文/附件、学生列表 `submitTime`、评分/评语和 `status=submitted` 合并 |
| `not_submitted_show_does_not_fabricate_status_or_submission_fields` | `assignmentStatus` 缺失 + sentinel submit-view 时不查学生列表，`submit_content=null`、附件空、`submitted_at=null` |
| `submit_view_failure_stays_redacted_and_bounded` | submit-view 失败仍返回详情；`submit_view_error` 非空、≤200 字符、token 已脱敏 |
| `empty_submit_view_keeps_detail_fields_and_reports_no_data` | HTTP 200 空 submit-view 保留详情正文，写入 `submit-view returned no data`，不把空响应当已交/未交结论 |
| `explicit_course_limits_the_list_lookup_without_fetching_courses` | `--course` 只查指定站点，不发课程列表 GET；仍取学生列表 `submitTime` |
| `detail_http_401_is_auth_3_without_retry_or_write` | 详情实际 HTTP 401 退鉴权 3，只读一次，零 upload/submit |
| `detail_transport_failure_is_network_4_without_retry_or_write` | transport failure 退网络 4，只读一次，零 upload/submit |
| `detail_http_503_is_upstream_5_without_retry_or_write` | 详情实际 HTTP 503 退 upstream 5，只读一次，零 upload/submit |
| `datetime_and_status_helpers_do_not_invent_missing_values` | `published_at` 分/秒归一化、缺失/非法时间不伪造；`submitted` / `overdue` / `pending` 三态 |

### 源内单元用例（`src/homework/verify.rs`）

| 用例 | 覆盖点 |
|---|---|
| `pass_compares_attachments_and_normalizes_text` | 可见提交 + 附件集合一致 + HTML/空白归一化正文一致；退出 0、`verified=true`、零 upload/submit |
| `mismatch_exits_6_after_two_read_only_attempts` | 附件和正文都不匹配时重试一次后退出 6，`failed_checks=["submit_view"]`，零 upload/submit |
| `second_attempt_can_confirm_eventual_consistency` | 首轮 sentinel 未可见、次轮出现可见提交和附件时退出 0，`attempts=2`，零 upload/submit |
| `sentinel_submission_is_not_treated_as_submitted` | `assignmentId=-1` 且无附件时 `has_submission=false`；列表未交时提交检查与列表检查都 failed |
| `submit_view_http_401_is_a_verification_error_exit_7_without_retry_writes` | submit-view 401 是校验 `error`（不是命令级 auth 3），退出 7、warning 非空、错误脱敏、零 upload/submit |
| `upstream_failure_text_is_redacted_and_bounded` | submit-view upstream/transport 文本先脱敏再截到 200 字符，退出 7，零 upload/submit |

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

读命令（`courses`、默认列表、`--course`、`--undone`、`show`、`verify`）共用 `homework::load_read_session` → `identity::require_ucloud_session`；凭据只从 `credentials.json` 读取（username + 非空密码 + role + session revision），不读取账号环境变量。已验证 session 需要 role；roleless/旧凭据在本地退 auth 3，不会创建或重写 session。`submit` 仍走单独的 `homework::load_session`：确认门禁通过后以“submit wiring 未实现”退 5，未接 transport；`download` 仍退 5；`--new` 在本地凭据检查后退 5。成功提交但未跑 `--no-verify` 时 helper 仍返回 6 且 `verification.status=not_run`，明确表示未核验，不伪造 `verified=true`；提交后的自动核验仍未接上本批的 `verify` helper。

结果里不序列化 detail 原始对象或 session 结构；接受的 `upstream.msg` 和错误文本在输出边界统一脱敏，`Session` 不实现 `Debug`。新增 fixture `homework_submit_echo_token.json` 让上游回显合成 token，供后续进程级 seam 复用；当前对应边界测试已在源内通过。

### Scaffold 边界

`src/homework/submit.rs` 的 multipart renderer 和固定 boundary 只用于隔离的 transport journal 测试。`upload_file` 目前整文件读入内存；在连接真实 transport 前必须替换为经过审查的 multipart encoder，并加上有界/流式读取。本批没有连接它；`submit` 公开 CLI 仍在确认门禁后返回 unsupported，upload/submit 路由也没有加入 transport allowlist。

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

以下用例名都指 `tests/homework_replay/cases.rs` 中的草稿，**尚未运行**。其中 `courses` 分页/去重/终止、默认 all / `--course` 分页/排序/过滤/`--limit` 和空 undone 已由上面的 `read.rs` 源内 helper 用例覆盖；提交门禁、dry-run、确认一次写入、响应丢失四类已有 `submit.rs` 源内覆盖。草稿仍保留为后续完整进程级录制验收。

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
- `show` 的学生列表查询失败时 `submitted_at=null` 尚无单独 fixture；`--course` 限制站点查找、无 `--course` 的目标课程缩窄和 submit-view 失败/未交/空对象已覆盖。
- `preview_filename` 已覆盖百分号编码、`filename*` RFC 5987 和引号；文件名内部的分号等更复杂变体未覆盖。
- verify 的空正文跳过正文检查、非网络意外异常、expected/observed 文本 200 字截断未单独覆盖；sentinel `-1`、无 `--text`、第二轮才通过、附件/正文失配和 401/upstream 错误已覆盖。
- verify 草稿仍带公开 CLI 不存在的 `--course`；接线前要按详情目标课程重做参数与录制，不能为了草稿直接新增该参数。
- 固定 5 秒重试等待只通过注入 sleep 验证“重试一次”的次数；没有墙钟等待断言。
- 上传失败、submit-view 401 后的续期、真实校园网上游验收未运行。真实提交/上传不在本阶段执行，校园网验收需用户单独确认。
- 真实非空 undone 的课程富化（`_enrich_undone_courses`）仍延期：`get_all_homework` 已实现，但尚未把全部课程作业按 activity id 回填到 `siteId=-1`/空 `siteName` 的 undone 行；需要真实样本和预算语义后再声称 parity。
- `--new DAYS` 延期（见上）：详情补查、30 次预算、`publish_time` 统计、`unknown_publish_time`、note、发布时间倒序和 `truncated` 均未实现；不做“只看列表自带发布时间”的半套过滤，以免静默丢掉无法定日期的作业。
- `download` 延期：详情/preview URL 读取层已有，但保存目录、`--force`、无 URL 跳过计数、文件名来源、CDN 下载 host 策略（不带 Blade-Auth）和零写入断言尚未接公开 CLI。
- 公开 `show` / `verify` / 默认列表 / `--course` 仍需父会话在 `cli.rs::uses_campus_transport` 与 `transport/cas_wire.rs` 放开路由；本 worktree 不修改这两个共享文件。
- Python UCloud 401 invalidate+retry 一次的续期行为延期到 identity session 适配器；当前只读 helper 明确只请求一次并退鉴权 3。
- 用户提供的真实只读基线（本 worktree 未重跑）显示课程首屏 11 行且 site id 唯一；自动化只用合成 fixture，真实校园网读取验收仍需用户单独确认。

## 下一批接线顺序

1. 父会话/整合者放开共享路由：`cli.rs::uses_campus_transport` 允许 `homework` 的默认列表、`show`、`verify`、`--course`；`transport/cas_wire.rs` 增加 GET `/ykt-site/work/detail`、`/ykt-site/work/submit-view`、`/blade-source/resource/preview-url`，以及 POST `/ykt-site/work/student/list`（允许 JSON body + `Content-Type: application/json`）。现有 `/ykt-site/site/list/student/current`、`/ykt-site/site/student/undone`、`/ykt-basics/oauth/token` 不变。
2. 接 `--new`：先按列表自带 publish time 分类；对缺失行按“截止时间新到旧”花最多 30 次详情查询，失败不消耗预算；输出 `publish_time{looked_up,resolved,unknown,failed,lookup_limit}`、`unknown_publish_time`、note、发布时间倒序和 `truncated`；auth 错误必须保持 auth 3。
3. 接 `download`：复用 detail/preview 读取，补保存目录/`--force`/跳过计数和 CDN host 策略；下载失败不得写半份文件。
4. 把提交 helper 接到 fixture/进程级 seam（含 `homework_submit_echo_token.json`），提交后自动核验复用本批 `verify` helper，不能复制第三套读取逻辑。
5. 真实非空 undone 富化、401 续期、真实校园网读取/提交和双平台验收单独执行，需用户确认。

## 当前验证状态

- `cargo fmt --check`：通过。
- `cargo test --locked --offline --lib`：151 个通过（含 31 个 homework 只读 helper、11 个 homework 提交 helper、6 个 detail、9 个 show、6 个 verify、3 个 text，以及 identity/transport/CAS 等其它 lane）。
- `cargo test --locked --offline --test contract_homework`：16 个通过。
- `cargo test --locked --offline`：全绿；151 单元 + 25 academics + 33 campus + 16 homework + 28 identity + 23 lifecycle 契约用例全部通过。
- `tests/homework_replay/cases.rs`：不在 Cargo 自动测试集合中；`run_scenario` 未接入，因此 30 个草稿全部不可运行，也没有伪装通过。
- 本批仍是 synthetic-only slice：公开读命令已接真实 `require_ucloud_session`，但只在进程内用合成 session/`Scripted` 验证；`--new`、`download`、`submit` 写入仍未接；未运行真实校园网、浏览器会话、上传或提交，也没有新增环境变量回放开关。
