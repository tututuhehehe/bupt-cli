# 教务与课表迁移测试清单（academics）

Python 基准：`d7c35ef`。合同：`docs/rust-migration.md`。本清单只覆盖 timetable/schedule、exams、scores、classrooms 的公开行为；不搬运 Python 私有函数或内部数据结构。

## 测试文件与夹具

- `tests/contract_academics.rs`：黑盒 CLI 用例，使用共享 `mod contract_support;`。
- `tests/fixtures/contracts/academics/*.html`：脱敏、合成的研究生课表/教学日历/考试表格。已通过 `include_str!` 编入 `src/academics/graduate.rs` 的解析单元测试；进程黑盒用例仍不启动浏览器、不访问校园服务。
- `tests/fixtures/contracts/academics/mobile_jwgl_*.json`：脱敏、合成的本科 mobile JWGL 外壳与响应（curriculum 完整/仅当前周、exam 三个接口、termGPA、todayClassrooms）。通过 `include_str!` 编入 `src/academics/mobile_jwgl.rs` 和 `src/academics.rs` 的注入用例；不读取真实账号、token 或响应。
- 测试凭据、密码、账号、课程、教师、房间均为合成值；不读取真实 `~/.bupt-cli`、浏览器 profile 或校园服务。

## 关键约定

1. **课表快照归属与当前学期**：Rust 快照格式为 v2，顶层带 `username` 和 `role`。只有 `username` 等于当前凭据、`role` 等于当前身份、且运行当天落在快照教学周日期的闭区间内，才可直接服务 `schedule` / `schedule calendar` / `schedule export`。v1 没有 `username`，属于“无法确认归属”，不能按 `role` 猜测后继续。归属不符、缺失、损坏或当天不在学期内的快照都走唯一一次自动刷新；刷新失败/不完整/不覆盖当天时保留旧文件但不把它当作当前学期，也不生成日历或导出。
2. **研究生刷新边界**：`schedule update` 和自动刷新通过 `TimetableSource` 进入 `ChromePageSource`；浏览器 Cookie 只由 identity 的 `load_cas_cookies_for_auth` 提供，复用同一份 owner/revision/私有文件校验和已维护的 cookie jar。academics 不再手读 `session.json`，也不使用密码值、不破解验证码。没有可用会话、revision 不符、文件权限过宽或 Cookie 过期时，都在启动 Chrome 前以 auth（退出 3）失败。导出的元数据只面向 HTTPS `auth.bupt.edu.cn`，保留 host-only 标记与 path/secure/httpOnly/expiry；`.bupt.edu.cn` 收窄到 auth host。测试通过 `GradPageSource` 注入合成 HTML，从不启动真实浏览器。
3. **缓存位置**：沿用旧行为，fixture 写到 `BUPT_CLI_CACHE_DIR/timetable.json`；凭据写到 `BUPT_CLI_CONFIG_DIR/credentials.json`；CAS 会话写到 `BUPT_CLI_CACHE_DIR/session.json`。
4. **网络隔离边界**：共享 `Sandbox` 只隔离 HOME/config/cache。把 `HTTP(S)_PROXY`/`ALL_PROXY` 指向关闭的本地端口是 best-effort，只对遵守这些环境变量的 HTTP 客户端生效；它不覆盖浏览器/非 HTTP 流量，也不能证明“没有发起网络请求”。真正的 no-calls 断言放在实现侧的注入来源用例中。
5. **动态日期与时钟**：进程用例的教学周围绕本机当天生成，避免测试在学期结束后因“过期”而误判；实现侧用例使用固定日期 `2026-09-23`，并显式把 `today` 传给 `select_snapshot`。运行时使用本机日期，无法确定本机日期则报告错误。
6. **凭据来源**：只用临时目录里的 `credentials.json`；账号环境变量不读取、不覆盖 JSON。role 只用于需要身份路由的教务操作。
7. **本科 mobile JWGL 只读边界**：`MobileJwglClient` 只通过注入的 `Transport` 发固定方法/路径/查询：`GET /bjyddx/student/curriculum`、`GET /bjyddx/kaowu/get_kszc`、`GET /bjyddx/kaowu/getKsapcx`、`GET /bjyddx/kaowu/findKcksapcx`、`POST /bjyddx/student/termGPA`（空 body）、`GET /bjyddx/todayClassrooms?campusId=01|04`；主机固定 `https://jwglweixin.bupt.edu.cn`。认证只用小写 `token` 请求头，不提交密码、不调用 `/bjyddx/login`、不读取 CAS Cookie/`session.json`。token 由 identity 的窄访问器提供：复用有效 token，必要时在固定 HTTPS 登录入口上获取一次。缺少绑定会话时，在查询前报错。
8. **本科日期与快照完整性**：日期只取 `SjdDay.mxrq`，`zc` 只作为教学周键，`xqid` 是 1-based 星期；绝不用“当前周 + 偏移”补缺失日期。`fetch_undergraduate` 的 Python 基准会返回它拿到的周网格；Rust 在写缓存前仍执行完整校验（第 1 周起、连续、每周 7 个日期、覆盖当天、所有课程可导出 ICS）。因此只返回当前周的响应会保留旧快照，而不是写半份日历。课程只读 `week.courses` 平铺数组；SDK 的嵌套 `item` 网格不进入 Python `fetch_undergraduate`，本实现也不重复纳入。字段保持 Python 语义：`period_start`/`period_end` 为 `null`，`campus` 为 `""`（SDK 课程行没有该字段），`credit` 缺失为 `null`，`fraction` 始终转成字符串。

## 用例映射

| Rust 用例 | Python 来源 | 覆盖点 |
|---|---|---|
| `invalid_arguments_are_usage_errors_without_credentials_or_cache` | `test_cli_errors.py::test_invalid_numeric_arguments_are_usage_errors`；合同“非法参数在读写业务状态、联网或启动浏览器前拒绝” | 非法日期、非法周次、缺参数、未知参数、非法校区；无凭据/无缓存时退出 2，不生成缓存 |
| `invalid_arguments_leave_an_existing_snapshot_untouched` | 同上 | 已有快照时非法参数不读写业务状态，快照字节不变 |
| `removed_schedule_exams_alias_is_a_usage_error_without_side_effects` | 合同“不提供 `schedule exams` 旧别名” | `schedule exams` 退出 2，不刷新缓存 |
| `cached_schedule_serves_only_the_matching_account_and_role` | `test_timetable.py::test_occurrences_resolve_weekday_and_date`、`test_timetable.py::test_occurrences_respect_the_range`；`test_courses.py` 缓存命中用例 | 匹配的 v2 快照按 `--date` 返回单日课程；`from_cache=true`；role/date/week/time/teachers/rooms 正确；不重写缓存 |
| `cached_calendar_is_week_ordered_and_monday_anchored` | `test_timetable.py::test_build_calendar_is_monday_anchored`、`test_week_of_and_day_range` | `schedule calendar` 输出按周排序、周一到周日、日期数量正确；不重写缓存 |
| `cached_week_query_resolves_real_dates_and_inclusive_range` | `test_timetable.py::test_occurrences_range_is_inclusive_on_both_ends` | `--week N` 使用真实日期表，范围两端包含；只返回该周课程 |
| `cached_export_writes_an_anonymized_ics_with_real_dates` | `test_timetable.py::test_build_ics_emits_one_event_per_session`、`test_build_ics_is_crlf_and_folded` | `schedule export` 每个 occurrence 一个 VEVENT；真实日期、开始/结束时间、SUMMARY/LOCATION、CRLF、折叠宽度、末周课程也导出 |
| `ics_folds_long_utf8_lines` | `test_timetable.py::test_build_ics_is_crlf_and_folded`、`test_grad.py::test_build_ics_is_crlf_and_folded` | 长中文 SUMMARY 按 UTF-8 八位字节折行，每行不超过 76 字节；按 RFC 5545 去折行后必须逐字节还原完整 SUMMARY，拒绝截断或省略 |
| `export_succeeds_even_when_opening_the_folder_fails` | 合同“导出文件成功不因打开系统文件管理器失败变成导出失败” | Unix 下伪造 `open` 写 sentinel 后退出 9：证明 opener 确实被调用，同时 `schedule export` 仍成功且文件存在；真实 Windows reveal 仍未实测 |
| `export_ignores_planted_temp_symlink_and_preexisting_temp_file` | 安全临时文件；合同“导出失败不覆盖旧文件” | `output.ics.tmp` 先放普通文件再换成指向 victim 的 symlink：两次导出都成功，普通临时文件内容不变、symlink 仍是链接且 victim 未变；证明实现不再用可预测临时路径 |
| `passwordless_profile_cannot_read_a_matching_snapshot` | 合同“`logout` 清掉本地密码和会话，保留课表”；身份/快照归属 | 密码被清掉但 username/role 仍匹配时，query/calendar/export 全部 auth 拒绝；快照字节不变，不生成 ICS |
| `environment_credentials_do_not_fill_a_passwordless_profile` | 旧凭据优先级的有意变更 | 配置缺密码时，即使合成 `BUPT_USERNAME` / `BUPT_PASSWORD` / `BUPT_ROLE` 全部存在，也不能补足本地 profile；查询停在 auth，快照不变 |
| `environment_credentials_do_not_override_saved_account_or_role` | 旧凭据优先级的有意变更 | 有效 JSON 的学号和 role 优先于冲突的账号环境变量；匹配的 graduate 快照仍可读取，role 输出保持 `graduate` |
| `date_arguments_require_ascii_yyyy_mm_dd_and_valid_year` | 合同“非法参数在读写业务状态前拒绝” | 带符号年份/分量、0000 年、非 ASCII 数字、非补零月日、非法闰日全部 usage；不触碰缓存 |
| `date_outside_the_current_term_reports_the_range_without_switching_terms` | 合同“`--date` 只筛选当前学期，范围外报告区间，不切换学期或当作没课” | 合法但不在当前学期的 2024-02-29 退出 2；错误同时报告请求日期和当前学期起止；不刷新、不改写快照 |
| `export_rejects_invalid_start_or_end_clocks_and_preserves_an_existing_file` | 合同“不伪造考试/课表事实”；完整 ICS 校验 | 25:00 开始时间、09:60 结束时间不再是“跳过事件”，而是整次导出失败；错误点名课程和对应的开始/结束时间，已有 `.ics` 字节不变，无临时文件残留 |
| `export_normalizes_recognizable_times_and_allows_missing_teacher_or_room` | 合同“导出先规范化上游可识别的日期/时间；教师/教室缺失允许” | `8：00` 与 `0935` 规范化为 08:00/09:35；课程缺 teachers/rooms 仍生成事件，且不写空 LOCATION |
| `export_rejects_missing_calendar_dates_and_names_the_course` | 合同“任一上课场次无法生成有效事件时点名课程，不生成半份日历” | 课程引用不存在的第 99 周日期：退出 5，错误含课程名和缺失日期，已有 `.ics` 不变 |
| `export_counts_real_events_and_escapes_injected_line_breaks` | 安全 ICS 导出 | 课程名/房间中的 CRLF、裸 LF、`BEGIN:VEVENT` / `END:VEVENT` 被转义成文本；物理事件边界仍各只有一条，`events` 不按子串计数 |
| `corrupt_and_wrong_version_caches_require_an_explicit_update` | `test_timetable.py::test_load_rejects_corrupt_or_wrong_version` | 损坏 JSON、未来版本、缺 `courses` 的快照不导致 panic/假成功；缺会话时停在 auth 拒绝，不重写缓存 |
| `unattributed_v1_cache_is_refused_instead_of_guessed` | 合同“不能确认旧快照归属时提示刷新” | v1 有 `role` 但无账号：query/calendar/export 全部拒绝并提示登录/刷新，不生成导出文件 |
| `cache_owned_by_another_account_is_refused` | 合同“只读当前学号及身份的可用课表快照” | v2 快照属于另一学号：三种读取命令全部拒绝，不泄漏/导出旧账号课表 |
| `cache_for_another_role_is_refused` | 合同同上；`test_cli_errors.py` 角色守卫 | v2 快照 role 与当前身份不符：三种读取命令全部拒绝，不泄漏旧身份课表 |
| `missing_snapshot_and_session_report_auth_without_fabricating_cache` | 合同“上游不可用时显式失败，不伪造数据” | 没有快照且没有 CAS 会话：自动刷新在启动浏览器前以 auth 退出 3，错误说明需要 `bupt login`，不创建缓存 |
| `manual_update_without_a_session_preserves_the_existing_snapshot` | 合同“刷新失败保留旧快照” | `schedule update` 强制刷新，但缺 CAS 会话：退出 3，旧快照字节不变，无临时文件 |
| `graduate_exams_without_a_session_is_an_auth_error_before_browser` | 合同“显式取数命令在浏览器前完成会话校验” | 研究生 `exams` 缺会话：退出 3，不写 cache/session，不启动浏览器 |
| `undergraduate_mobile_commands_require_a_bound_session_before_network` | 本科查询的身份边界 | 无绑定会话时，`exams`、`scores`、`classrooms` 和 `schedule update` 不创建会话、不发起查询 |
| `out_of_range_week_points_at_explicit_update_and_keeps_the_snapshot` | `commands.cmd_schedule` 中 `--week` 不存在的分支 | 合法整数但不在快照范围：退出 2，hint 指向 `schedule update`，缓存不变 |
| `invalid_weekday_entries_are_ignored_not_wrapped_or_fabricated` | `test_timetable.py::test_occurrences_ignores_out_of_range_weekdays` | 手改缓存中的 `-1`/`7`/`99` 不崩溃、不绕到周日、不伪造课程；返回 count 0 |
| `graduate_accounts_are_refused_by_undergraduate_jwgl_commands` | `test_cli_errors.py::test_undergraduate_jwgl_commands_refuse_graduate_accounts` | `scores`/`classrooms` 对研究生身份在联网前退出 2，说明走本科教务；不生成 session/cache |
| `role_guards_only_refuse_graduate_accounts_not_unknown_passwords` | `test_cli_errors.py::test_undergraduate_role_is_not_blocked` | 缺密码时 `exams`（研究生）/`scores`（本科生）都停在 auth，而不是被本科教务守卫误拒；不写 session/cache |
| `unsupported_role_is_rejected_before_backend_selection` | 合同“登记显式选择的本科/研究生身份” | 未知 role 不被猜测为本科/研究生，命令在选后端前退出 2；不触碰缓存/session |

## 实现侧当前学期、刷新与安全写入用例

这些用例在 `src/academics.rs` 的私有 `#[cfg(test)]` 模块里，通过 `select_snapshot(..., today, ..., &mut FakeSource)` 或 `GraduatePageTimetable` 直接注入日期和页面来源；不启动进程、不联网、不依赖真实时钟。

| Rust 用例 | 覆盖点 |
|---|---|
| `fresh_owned_snapshot_is_served_without_fetching` | 当天在归属正确的学期闭区间内：来源调用 0 次，`from_cache=true`，快照不被重写 |
| `freshness_includes_both_term_endpoints_and_ignores_fetch_age` | 学期首日与末日都算新鲜；`fetched_at` 即使很旧也不触发刷新（不是 TTL），且不重写快照 |
| `missing_snapshot_fetches_exactly_once_and_writes_the_new_term` | 缺快照：来源恰好 1 次，写入带当前学号/身份的新学期快照 |
| `stale_snapshot_fetches_exactly_once_and_replaces_the_cache` | 学期已不覆盖当天：来源恰好 1 次；选中并缓存新课程，而不是旧学期 |
| `foreign_snapshot_is_never_used_even_when_refresh_fails` | 归属不符且刷新失败：返回错误，不把外来快照当兜底，旧文件不变 |
| `foreign_snapshot_is_replaced_only_after_a_clean_refresh` | 归属不符但刷新成功：恰好 1 次取数，只有刷新成功后才替换缓存 |
| `date_outside_the_selected_term_never_fetches_again` | 先用新鲜快照完成选择（0 次），再由 `ensure_date_in_term` 拒绝范围外 `--date`，来源仍为 0 次 |
| `failed_incomplete_or_out_of_term_refresh_preserves_the_old_snapshot` | 刷新报错、缺时钟、不覆盖当天、或“零课程 + 畸形/空教学周日历”：都只取 1 次，旧快照字节不变，不因 `term_bounds` 跳过坏日期而部分替换 |
| `forced_update_fetches_even_for_a_fresh_snapshot_and_preserves_on_failure` | `update` 语义强制取数（即使快照新鲜）；成功后替换，失败后保留旧快照 |
| `injected_graduate_pages_refresh_a_complete_current_snapshot` | 通过 `GraduatePageTimetable` 注入研究生课表/完整 1..19 周日历：只取一次、请求 Schedule+Calendar、写入 `role=graduate`/`source=yjxt`/19 周完整快照 |
| `malformed_graduate_calendar_preserves_the_previous_snapshot` | 注入只有首末两周的日历（课表引用第 4..19 周）：刷新校验失败，旧快照字节不变，不生成半份日历 |
| `exams_command_uses_injected_pages_and_keeps_the_empty_marker_explicit` | 注入考试表格：有行时保留课程/日期/时间/地点/备注；空标记时 count=0 且 note 明确，不伪造考试 |
| `undergraduate_exams_are_refused_before_the_graduate_page_source` | 本科身份不调用研究生页面来源（0 次），停在 pending/usage 路由 |
| `mobile_curriculum_refresh_uses_sjd_dates_and_writes_one_complete_snapshot` | 注入完整两周课程/日期：只调用一次 curriculum；写入 `role=undergraduate`/`source=mobile_jwgl`/真实 `mxrq`/`period=null`；学号和日期不被猜测 |
| `mobile_current_week_only_response_never_replaces_the_old_snapshot` | 注入只有第 3 周的网格：完整校验拒绝“首周不是 1”，旧快照逐字节不变；不按偏移补第 1、2 周 |
| `mobile_exam_command_uses_three_reads_and_keeps_raw_rows` | 注入三个考试接口：按顺序读取、`token` 三次复用、`weeks`/`exams`/`course_arrangements`/note 字段保持 Python 形状 |
| `mobile_scores_and_classrooms_commands_preserve_filters_and_campus_query` | 注入 termGPA/todayClassrooms：学期过滤、`count`、空 credit、平均分、GPA 原始条目、校区查询参数与容量 null 语义 |
| `native_mobile_token_refusal_does_not_retry_or_echo_the_response` | 生产 token 被拒绝时不重复请求、不回显响应、不再次提交密码 |
| `atomic_writes_leave_predictable_temp_files_untouched` | 普通 `timetable.json.tmp` / `output.ics.tmp` 预置内容在写入后仍逐字节不变；实际写入使用随机 `NamedTempFile` |
| `atomic_writes_do_not_follow_planted_temp_symlinks` | 预置临时路径若是指向无关文件的 symlink：保存快照和导出都不跟随，victim 内容与链接目标都不变 |
| `atomic_write_refuses_a_symlink_target_without_touching_it` | 目标本身是 symlink 时拒绝替换；返回 upstream 错误，链接和指向文件都不变 |
| `concurrent_atomic_writes_leave_one_complete_file` | 多个并发写同一目标：最终内容是某一次完整 payload，不出现交叉/截断内容 |

## 实现侧研究生页面解析、Cookie 导出与浏览器边界用例

| Rust 用例 | 覆盖点 |
|---|---|
| `timetable_expands_rowspans_and_merges_consecutive_periods` | `rowspan` 展开、星期列、节次时钟、连续节次合并；两个合成课程字段正确 |
| `calendar_maps_chinese_weeks_and_crosses_the_new_year` | 中文周次、周一到周日、12 月/1 月跨年；不按运行日期猜 |
| `term_label_is_taken_from_the_calendar_year_and_never_guessed` | 学期标签靠近 `dgData`；缺标签/缺年份显式报错，不猜学年 |
| `missing_page_markers_are_errors_not_empty_results` | 缺星期表头、缺 `dgData`/周次时显式报错，不返回空课表 |
| `exams_empty_marker_keeps_the_header_and_invents_no_rows` | “没有信息数据”保留表头、count=0、不生成行 |
| `exams_real_row_preserves_every_reported_field` | 真实行保留课程、考核方式、日期、时间、地点、备注 |
| `malformed_exam_page_is_an_error_not_an_empty_schedule` | 缺“考试课程”表头是 upstream 错误，不伪装成“没有考试” |
| `snapshot_from_pages_requires_both_pages_and_a_term_label` | 缺课表/缺日历/缺学期标签都失败；不会只保存半份快照 |
| `cas_cookie_export_is_bound_and_preserves_host_scope` | 有效合成会话经 identity 导出：host-only 为 true；domain Cookie 保留 path/secure/expiry，并收窄到 `auth.bupt.edu.cn`；`Debug` 不含 Cookie 值 |
| `cas_cookie_export_rejects_a_different_credential_revision` | 凭据 revision 与会话不一致：在浏览器前 auth 失败 |
| `cas_cookie_export_rejects_a_public_session_file_before_browser` | `session.json` 为 0644：在浏览器前以 auth 拒绝，不读取/注入 Cookie |
| `cas_cookie_export_rejects_expired_cookies` | 过期 Cookie 不进入浏览器；没有可用 Cookie 时 auth 失败 |
| `yjxt_redirect_requires_the_exact_https_host` | 登录后跳转用解析后的 HTTPS 精确 host；`yjxt.bupt.edu.cn.evil.example`、外部 query 子串、HTTP、非标准端口、userinfo 都拒绝 |
| `chrome_cookie_params_preserve_host_only_scope` | host-only Cookie 传 Chrome 时省略 `domain`、使用 auth host URL，并保留 path/secure/httpOnly |
| `chrome_cookie_params_keep_domain_cookie_scope_and_expiry` | 非 host-only Cookie 使用 canonical auth domain，并保留 path/expiry |
| `menu_urls_stay_on_known_graduate_pages` | 菜单绝对 URL、协议相对 URL、`..`、反斜杠都拒绝；只拼已知研究生页面 |
| `menu_parser_and_eid_extraction_ignore_untrusted_urls` | 菜单 name/url 提取；EID 只接受安全字符 |
| `websocket_handshake_helpers_match_known_vectors` | 自实现的 SHA-1/Base64 对已知向量正确，避免新增依赖 |

## 实现侧 mobile JWGL 客户端与映射用例

这些用例在 `src/academics/mobile_jwgl.rs` 的私有 `#[cfg(test)]` 模块里，用注入的 `Scripted` 传输和合成 JSON 夹具运行；不联网、不使用真实 token。

| Rust 用例 | 覆盖点 |
|---|---|
| `curriculum_fixture_uses_sjd_dates_and_preserves_python_nulls` | 真实 `SjdDay.mxrq` 进入日历；课程去重后两门；`period_start`/`period_end` 为 null，`campus` 为空，教师/房间字段保留 |
| `curriculum_without_dates_is_an_upstream_error` | 只有课程没有日期：退出 5，绝不按周次补日期 |
| `curriculum_uses_only_sjd_dates_and_never_invents_missing_days` | 只有第 3 周的响应仍只映射第 3 周；缺失的周不会被补 |
| `week_expansion_matches_python_week_specs` | 单双周、明细回退、括号备注、反向区间不展开；结果排序去重 |
| `semester_and_end_time_fallbacks_match_the_sdk` | `semesterId` 为空时用 `xnxq01id`；`endTIme` 为空时用 `endTime` |
| `empty_curriculum_is_an_upstream_error` | 成功外壳但 `data=[]`：退出 5，提示没有课表数据 |
| `exam_client_is_lazy_and_preserves_raw_rows` | 三个考试接口顺序读取；token 只取一次；`token` 头复用；行/周次原样保留 |
| `scores_payload_filters_semester_and_preserves_optional_fields` | 学期过滤、`count`、平均分、GPA 原始条目、空 credit => null、数字 fraction 转字符串、空学期过滤等价于不过滤 |
| `classrooms_payload_preserves_raw_text_and_capacity_nulls` | `CLASSROOMS` 优先、原始文本保留、空/非数字容量 => null、空项跳过 |
| `classrooms_query_uses_the_allowed_campus` | `campusId=01` 查询参数精确，路径固定 |
| `auth_failure_happens_before_transport` | 无 token 时传输调用 0 次 |
| `empty_or_expired_response_retries_once_via_the_auth_seam` | 空响应触发一次 `invalidate` 后重试；第二次用新 token；不再循环 |
| `permission_denial_is_not_retried` | `未授权访问` 是永久权限边界，退出 5，不刷新 token |
| `token_debug_redacts_the_value` | `MobileJwglToken` 的 `Debug` 不出现 token 值 |

## 实施状态（研究生与本科刷新边界）

- `NativeTimetableSource` 已把 `schedule` / `schedule calendar` / `schedule export` / `schedule update` 的刷新边界接到 `ChromePageSource`。研究生路径启动隔离的已安装 Chrome（临时 profile、正常桌面 UA、仅访问已知 `auth.bupt.edu.cn` / `yjxt.bupt.edu.cn` / `TermCalender.aspx`），通过 CDP 读取渲染后的 HTML；不提交密码、不破解验证码，也没有 stealth/自动化隐藏参数。
- `ChromePageSource` 通过 `load_cas_cookies_for_auth` 取得 Cookie；identity 侧与 `load_owned_cas_session` 共用 `load_bound_session`（config/cache 路径 helper），校验账号、role、revision、私有目录/普通文件，并用 `cookie_store` 的已维护 jar 校验 domain/path。返回值是窄的 `CasCookie` 元数据：只面向 HTTPS `auth.bupt.edu.cn`，保留 host-only、path、secure、httpOnly、expiry，`Debug` 对值打码；`.bupt.edu.cn` 后缀收窄到 auth host。会话缺失/过期/归属或 revision 不符/权限过宽都在 Chrome 前 auth 失败（退出 3）。
- 本科路径已接入 `MobileTimetableSource`：通过注入传输读取 `get_curriculum`，只从 `SjdDay.mxrq` 生成日历，课程字段按 Python `fetch_undergraduate` 映射（周次/星期/时钟/教师/房间；`period_start/end=null`，`campus=""`）；完整学期校验与研究生共用，因此只返回当前周的响应会保留旧快照。
- `exams` 的研究生路径已接通；本科 `exams` 现在走 mobile JWGL 三个读取接口，保留 `weeks`/`exams`/`course_arrangements`/`note` 的 Python 形状。`scores` 走 `termGPA` 的学期分组/过滤；`classrooms` 走 `todayClassrooms`，`--campus` 仍只允许 `01`/`04`。
- `mobile_auth` 已接入 `identity::require_mobile_jwgl_token`，本科查询和强制刷新均使用归属校验后的 token；academics 不重复解析 `session.json`。查询拒绝 token 时报告 auth，不盲目重试或再次发送密码。
- 本地优先读取路径（`schedule` / `schedule calendar` / `schedule export` 命中新鲜快照）不构造浏览器、不读会话；实现侧注入用例对刷新来源的调用次数有明确断言。
- 缓存与 `.ics` 共用原子写入助手：在目标同目录用 `NamedTempFile` 创建不可预测临时文件，写满、flush、`sync_all` 后才 `persist`；目标若是 symlink、非普通文件或与临时文件同一 inode 会拒绝。`tempfile` 是正式依赖，本轮未增加 crate，`Cargo.lock` 无变化。
- 本清单进程用例 32 个；academics 实现侧用例 50 个（17 个当前学期/刷新/写入 + 8 个研究生解析 + 6 个浏览器边界 + 14 个 mobile 客户端/映射 + 5 个本科命令注入），另有 identity session 的 4 个 Cookie 导出用例。`cargo fmt --check`、`cargo test --locked --offline --lib`、`cargo test --locked --offline --test contract_academics` 是交付检查项。
- 当前学期策略：只看运行当天是否在快照日期闭区间内，不看 `--date`、不看 TTL、不按抓取时间淘汰；学期内不会因快照“旧”而自动刷新。跨学期时按上述边界自动刷新一次。

## 尚未完成 / 交接

### 1. 生产接口清单

- 主机：`https://jwglweixin.bupt.edu.cn`；请求头固定 `token: <cached token>`、桌面 UA、`Accept: application/json, text/plain, */*`；所有请求为空 body，不发送 CAS Cookie 或密码。
- `GET /bjyddx/student/curriculum`（无 query）
- `GET /bjyddx/kaowu/get_kszc`（无 query）
- `GET /bjyddx/kaowu/getKsapcx`（无 query）
- `GET /bjyddx/kaowu/findKcksapcx`（无 query）
- `POST /bjyddx/student/termGPA`（无 query、空 body；虽然方法为 POST，语义是只读）
- `GET /bjyddx/todayClassrooms?campusId=01|04`
- `MobileJwglClient` 已把路径写死并按上述顺序调用；生产传输只放行列出的业务路径，包括只读空 body 的 `termGPA` POST。`Get_sjkbms`、`noticeList` 本轮未使用；登录 `/bjyddx/login` 不由本客户端调用。

### 2. 真实上游验收（本轮未做）

- Python SDK 明确说 curriculum 接口通常只描述当前周；`zc` 是否从 1 开始、是否一次返回整学期都未在真实本科生账号验证。Rust 的完整校验会拒绝“只有当前周”的刷新并保留旧快照；若真实响应确实只有当前周，需要另做 DOM/日期来源适配，不能按周次补日期。
- `termGPA`/成绩相关接口可能返回 `{"code":"0","Msg":"未授权访问：..."}`；客户端把它归为永久权限边界（退出 5、不重试）。需要真实账号确认成绩读取是否在权限范围内。
- 三个考试接口在学期之间可能返回空 data；已保留空数组和 note，不伪造考试。`todayClassrooms` 是实时数据，没有日期参数。
- 真实 Chrome/CDP/校园网验收仍单独安排；本轮明确不启动真实 Chrome、不访问真实凭据/浏览器 profile/校园服务。CAS Cookie 过期、瑞数挑战、Chrome 版本差异、真实菜单/课表/日历/考试页面结构都需联网记录；Python 基准里已知的 HTTP service callback 路径仍未确认，不据此制造新路由或修改 Chrome 隐私设置。

### 4. 平台与解析边界

- Unix `0600` 权限、Windows 下打开导出目录 / persist 行为仍未验证。
- 真实课表/考试页面结构若改版，应显式失败而非猜测；真实验收不得混入离线测试。
- 移动教务 JSON 的未知字段保持原始/空值语义；嵌套 `item` 网格不进入本科课表缓存，避免与 Python `fetch_undergraduate` 的平铺 `courses` 行为分叉。
