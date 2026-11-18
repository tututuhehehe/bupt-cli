# 作业与课程迁移测试（homework lane）

对照 Python：`d7c35ef`。本 lane 负责 `courses` 与 `homework`（默认列表 / `show` / `download` / `submit` / `verify`），对应 Python 测试：

- `tests/test_courses.py`
- `tests/test_homework_new.py`
- `tests/test_homework_show.py`
- `tests/test_verify.py`

Rust 用例全部位于 `tests/contract_homework.rs`，只通过公开 CLI 断言退出码、stdout/stderr 文档、字段和不发生的副作用。没有调用 Python 私有函数，也没有把 Python 数据结构搬进 Rust。

## 有意变化

- `bupt homework` 是唯一列表入口；旧别名 `bupt homework list` 不再存在。`homework` 仍接受 `--undone` / `--course` / `--new` / `--limit`。
  - `homework_defaults_to_the_list_command`：无凭据时 `homework` 已越过解析器进入会话层（退出 3），而不是报“需要子命令”。
  - `explicit_homework_list_alias_is_rejected_as_usage`：`homework list` 在读写状态、联网前退出 2。
- 所有非交互输出单个 JSON 文档，带 `schema: "bupt.v1"`；错误走 stderr。成功结果的 `ok` 字段保留。
- `--pretty` 只改变排版：`pretty_changes_layout_not_values` 对同一条录制比较解析后的完整文档。

## 测试缝：合成录制回放

共享 `contract_support` 只隔离家目录/配置/缓存，没有上游传输缝。为避免真实校园网、真实凭据和 `#[ignore]` 假用例，本 lane 的夹具使用一个最小测试缝：

```sh
BUPT_CLI_CONTRACT_REPLAY=/absolute/path/to/recording.json bupt ...
```

这是测试缝，不是公开业务参数。要求实现方（或整合者认可的等价缝）满足以下语义：

- 回放文件是单个 JSON 对象，字段：
  - `schema: "bupt.contract-replay.v1"`
  - `session`：合成会话，至少提供 `access_token`、`user_id`；回放模式不读取真实凭据、不登录。
  - `now`：固定时钟（本 lane 为 `2026-11-18T12:00:00`），用于 `--new` 窗口和 `open/overdue` 判断，避免测试随时间漂移。
  - `exchanges`：请求/响应录制数组。
- 每个 exchange 的 `request`：
  - `method` 必填；`path`（UCloud 相对路径）或 `url`（外部下载完整 URL）二选一。
  - 可选 `query`、`json`：列出的字段必须匹配，未列出的内部参数（例如分页 `size`）允许不同。
  - 上传是 multipart，`request` 只匹配 method/path。
- `response`：
  - `status`，以及 `json`（JSON 响应体）或 `body`（原始文本，用于附件下载）。
- 匹配规则：按文件顺序取第一条尚未消费的匹配请求；同一路径多次出现时按顺序消费。
- 未匹配请求必须让命令失败且绝不回退到真实网络；未消费的录制允许存在（同一录制可用于“跳过已存在文件”等分支）。
- 夹具只含合成账号、合成 token、`.invalid` 主机和合成正文，不含真实秘密。

## 覆盖矩阵

### courses

| Python 用例 | Rust 用例 | 覆盖点 |
|---|---|---|
| `test_courses_concatenates_pages`、`test_courses_deduplicates_across_pages` | `courses_concatenates_pages_deduplicates_and_stops_on_a_short_page` | 跨页拼接、保持页序、按 site id 去重、短页终止、总数 |
| `test_courses_keeps_every_empty_site_id` | `courses_keeps_every_empty_site_id` | 空 `siteId` 不能因去重被吞掉 |
| `test_courses_stops_on_a_short_page` | `courses_stops_on_a_short_page_without_a_second_request` | 短页后不再请求下一页 |
| `test_courses_stops_on_an_empty_page`、`test_courses_returns_empty_without_error` | `courses_stops_on_an_empty_page_without_a_second_request` | 空页立即结束，空结果不是错误 |
| `test_courses_respects_the_page_limit` | `courses_stops_at_the_safety_page_limit` | 10 页上限，异常上游不无限循环 |

### homework 列表 / 过滤 / `--new`

| Python 用例 | Rust 用例 | 覆盖点 |
|---|---|---|
| `test_list_without_new_never_calls_the_detail_endpoint` | `default_homework_list_sorts_by_deadline_and_exposes_missing_metadata` | 默认列表、截止时间排序、缺失截止时间排最后、缺失字段为 null、不查详情 |
| `test_new_keeps_only_recent_assignments`、`test_new_sorts_newest_publication_first`、`test_new_uses_list_publish_time_without_a_detail_call` | `new_filter_uses_list_publish_times_and_sorts_newest_first` | 列表自带发布时间时零详情请求、窗口过滤、发布时间倒序 |
| `test_new_discloses_assignments_it_could_not_date`、`test_new_reports_unknown_count_when_lookup_budget_runs_out` | `new_filter_reports_undated_assignments_and_respects_the_lookup_budget` | 30 次详情预算、逐条披露 unknown、`lookup_limit`、note |
| `test_new_looks_up_only_the_filtered_assignments` | `new_filter_looks_up_only_assignments_that_pass_the_filters` | `--course` + `--undone` 先过滤再花预算；已交作业不触发详情 |
| `test_new_reports_detail_failures`、`test_failed_lookups_do_not_consume_the_budget` | `new_filter_failed_lookups_do_not_consume_the_budget` | 详情瞬时失败计入 failed、不消耗预算、后续作业仍能解析 |
| `test_new_with_empty_list_says_so`、`test_new_says_so_when_nothing_falls_in_the_window` | `new_filter_explains_an_empty_list_and_an_empty_window` | “没有作业”与“全部有日期但都在窗口外”可区分 |
| `test_auth_failure_is_raised_not_reported_as_missing_publish_time` | `new_filter_auth_failure_is_not_reported_as_missing_publish_time` | 401 退 3 / `error.type=auth`，不伪装成 unknown |
| （过滤/排序回归） | `undone_filter_uses_the_undone_endpoint_and_sorts_by_deadline` | `--undone` 使用 undone 数据源并按截止时间排序 |
| （组合过滤回归） | `course_and_undone_filters_combine` | `--course` 优先取课程作业，再应用 `--undone` |

### 详情 / 下载

| Python 用例 | Rust 用例 | 覆盖点 |
|---|---|---|
| `test_show_submitted_uses_submit_view_and_matching_site`、`test_homework_show_reports_status` | `show_merges_detail_submit_view_and_student_list` | 详情、submit-view、同名课程站点、学生列表 `submitTime`、教师/学生附件、正文、评语、`status=submitted` |
| `test_show_not_submitted_skips_list_lookup`、`test_get_detail_assignment_status_99_means_not_submitted` | `show_not_submitted_does_not_fabricate_submission_fields` | 未交时不伪造正文/附件/提交时间，也不请求学生列表 |
| `test_show_succeeds_when_submit_view_raises` | `show_survives_submit_view_failure_and_redacts_the_error` | submit-view 失败仍返回详情结果；错误有界且脱敏 |
| （下载回归） | `download_saves_attachments_and_skips_entries_without_urls` | 预览 URL 解析、保存附件、无 URL 项计入 skipped |
| （下载回归） | `download_does_not_overwrite_without_force` | 已存在文件默认跳过；`--force` 才覆盖 |

### 提交门禁 / 校验输出边界

| Python 用例 | Rust 用例 | 覆盖点 |
|---|---|---|
| `test_submit_without_confirm_uploads_nothing` | `submit_without_confirmation_exits_two_before_upload` | 无 `--i-confirm-submit` 退 2；录制中没有 upload/submit，若先上传就会因未匹配请求失败 |
| `test_dry_run_uploads_and_submits_nothing` | `submit_dry_run_never_uploads_or_submits` | `dry_run=true`、计划文件/已有附件、无 verification、无 upload/submit |
| `test_dry_run_and_confirm_are_mutually_exclusive` | `submit_rejects_dry_run_with_confirmation_before_credentials` | 参数互斥在凭据/网络前退 2 |
| `test_submit_with_confirm_uploads_then_submits`、`test_submit_uploaded_file_is_the_expected_attachment_set`、`test_submit_verified_pass_exits_zero` | `submit_confirmed_uploads_submits_once_and_verifies` | 上传一次、提交一次、附件集合参与校验、退出 0、`verified=true` |
| `test_submit_no_verify_skips_all_readonly_checks` | `submit_no_verify_skips_read_only_checks` | `verification={status:skipped}`、`verified=null`、不请求提交后核验 |
| `test_submit_unverified_mismatch_exits_six` | `submit_unverified_exits_six_without_resubmitting` | 两次只读尝试后退出 6；输出失败检查细节；stderr 警告不要盲目重交；录制只有一个 submit，重交会失败 |
| `test_submit_verification_error_exits_seven` | `submit_verification_error_exits_seven_without_resubmitting` | 核验自身失败退出 7、`verified=null`、warning、stderr；录制只有一个 submit |
| `test_verify_subcommand_pass`、`test_verify_without_resource_id_accepts_visible_submission` | `verify_bare_accepts_a_visible_submission_and_normalises_text` | 只读 verify、无 `--resource-id` 时只要求可见提交、正文空白归一化 |
| `test_mismatch_on_both_attempts_is_failed`、`test_content_mismatch_when_text_expected`、`test_verify_subcommand_failure_exits_six`、`test_verify_with_resource_id_still_compares_exactly` | `verify_resource_mismatch_exits_six_with_the_read_only_hint` | 附件/正文严格比对、退出 6、verify-only hint 不声称“上游已成功提交” |
| `test_network_error_on_last_attempt_is_error`、`test_error_message_is_redacted_and_bounded` | `verify_error_exits_seven_without_submitting` | 末次尝试网络错误退 7、warning、错误文本有界且脱敏、无提交 |

## 未覆盖 / 延期

- `test_courses.py` 中 `test_fresh_cache_is_used_without_a_fetch`、`test_stale_cache_triggers_a_fetch`、`test_missing_cache_triggers_a_fetch` 属于课表/教务 lane，不在本 lane 重写。
- Python vendor 的同步/异步私有解析用例（`test_get_detail_string_zero_and_submit_info_compatibility`、`test_async_get_detail_assignment_status`、`test_async_find_list_record_matches`、`test_find_list_record_stops_at_first_site_with_match`）：不迁移私有函数。命令级已覆盖 `assignmentStatus=0/99`；字符串 `"0"`、`submitInfo.isSubmit`、异步实现属于内部兼容细节，等实现暴露可观察差异时再补。
- `show` 的空 submit-view 回退、学生列表查询失败时 `submitted_at=null`、`--course` 限制站点查找：行为已由实现保留，但没有单独的录制用例；需要补一个空 `data:{}` 与一个 `student/list` 500 的合成录制。
- `preview_filename` 的 RFC 5987 `filename*=UTF-8''`、引号、文件名内分号等变体：当前只覆盖普通 URL 编码文件名，解析细节仍待补。
- verify 的“第二次尝试才通过”、无 `--text` 时忽略正文、空正文跳过正文检查、sentinel `assignmentId=-1` 且无附件、意外异常（非网络）以及 expected/observed 文本 200 字截断：未单独建用例。
- 固定 5 秒重试等待只用 `attempts`/退出码验证，未做墙钟计时断言（避免 CI 抖动）；实现仍需遵守 5 秒语义。
- 上传失败、submit-view 返回 401 后的续期、真实校园网上游验收均未运行。真实提交/上传不在本阶段执行，后续校园网验收需用户单独确认。

## 当前验证状态

- `cargo test --offline --test contract_homework --no-run`：通过。
- 受控红跑：`cargo test --offline --test contract_homework -- --test-threads=1`，全部用例因空实现 `exit=78` 失败；没有编译错误、没有联网、没有 `#[ignore]`/`#[should_panic]`。
- `tests/contract_homework.rs` 通过 `rustfmt --check --config skip_children=true`。仓库级 `cargo fmt --check` 仍会报共享 `tests/contract_support/mod.rs` 既有格式差异；该文件属于整合者，本 lane 未修改。
