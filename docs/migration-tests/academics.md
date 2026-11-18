# 教务与课表迁移测试清单（academics）

Python 基准：`d7c35ef`。合同：`docs/rust-migration.md`。本清单只覆盖 timetable/schedule、exams、scores、classrooms 的公开行为；不搬运 Python 私有函数或内部数据结构。

## 测试文件与夹具

- `tests/contract_academics.rs`：全部黑盒 CLI 用例，使用共享 `mod contract_support;`。
- `tests/fixtures/contracts/academics/*.html`：脱敏、合成的研究生课表/教学日历/考试表格，供后续有注入缝时验证解析。当前没有被编译进测试，因为黑盒 CLI 还没有可注入页面来源的公开缝。
- 测试凭据、密码、账号、课程、教师、房间均为合成值；不读取真实 `~/.bupt-cli`、浏览器 profile 或校园服务。

## 关键约定

1. **课表快照归属**：用例假设 Rust 快照格式为 v2，顶层带 `username` 和既有 `role`。只有 `username` 等于当前凭据、`role` 等于当前身份时才可服务 `schedule` / `schedule calendar` / `schedule export`。v1 没有 `username`，属于“无法确认归属”，必须提示 `schedule update`，不能按 `role` 猜测后继续。
2. **缓存位置**：沿用旧行为，fixture 写到 `BUPT_CLI_CACHE_DIR/timetable.json`；凭据写到 `BUPT_CLI_CONFIG_DIR/credentials.json`。
3. **不联网**：所有用例通过共享 `Sandbox` 隔离 HOME/config/cache，并把 `HTTP(S)_PROXY`/`ALL_PROXY` 指向关闭的本地端口。这样即使实现意外尝试刷新，也不会到达真实校园网。
4. **动态日期**：快照的教学周围绕 UTC 当天生成（前后各数周），避免测试在学期结束后因“过期”而误判；断言的具体日期由同一套日期算法算出。

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
| `ics_folds_long_utf8_lines` | `test_timetable.py::test_build_ics_is_crlf_and_folded`、`test_grad.py::test_build_ics_is_crlf_and_folded` | 长中文 SUMMARY 按 UTF-8 八位字节折行，每行不超过 76 字节 |
| `export_succeeds_even_when_opening_the_folder_fails` | 合同“导出文件成功不因打开系统文件管理器失败变成导出失败” | Unix 下伪造 `open` 退出 9，`schedule export` 仍成功且文件存在 |
| `corrupt_and_wrong_version_caches_require_an_explicit_update` | `test_timetable.py::test_load_rejects_corrupt_or_wrong_version` | 损坏 JSON、未来版本、缺 `courses` 的快照不导致 panic/假成功；提示 `schedule update`；不重写缓存 |
| `unattributed_v1_cache_is_refused_instead_of_guessed` | 合同“不能确认旧快照归属时提示刷新” | v1 有 `role` 但无账号：query/calendar/export 全部拒绝，提示刷新，不生成导出文件 |
| `cache_owned_by_another_account_is_refused` | 合同“只读当前学号及身份的可用课表快照” | v2 快照属于另一学号：三种读取命令全部拒绝，不泄漏/导出旧账号课表 |
| `cache_for_another_role_is_refused` | 合同同上；`test_cli_errors.py` 角色守卫 | v2 快照 role 与当前身份不符：三种读取命令全部拒绝 |
| `out_of_range_week_points_at_explicit_update_and_keeps_the_snapshot` | `commands.cmd_schedule` 中 `--week` 不存在的分支 | 合法整数但不在快照范围：退出 2，hint 指向 `schedule update`，缓存不变 |
| `invalid_weekday_entries_are_ignored_not_wrapped_or_fabricated` | `test_timetable.py::test_occurrences_ignores_out_of_range_weekdays` | 手改缓存中的 `-1`/`7`/`99` 不崩溃、不绕到周日、不伪造课程；返回 count 0 |
| `graduate_accounts_are_refused_by_undergraduate_jwgl_commands` | `test_cli_errors.py::test_undergraduate_jwgl_commands_refuse_graduate_accounts` | `scores`/`classrooms` 对研究生身份在联网前退出 2，说明走本科教务；不生成 session/cache |
| `role_guards_only_refuse_graduate_accounts_not_unknown_passwords` | `test_cli_errors.py::test_undergraduate_role_is_not_blocked` | 缺密码时 `exams`（研究生）/`scores`（本科生）都停在 auth，而不是被本科教务守卫误拒；不写 session/cache |
| `unsupported_role_is_rejected_before_backend_selection` | 合同“登记显式选择的本科/研究生身份” | 未知 role 不被猜测为本科/研究生，命令在选后端前退出 2；不触碰缓存/session |

## 预期红（空实现）

`src/main.rs` 当前只向 stderr 打印 `Rust CLI not implemented; test migration scaffold only` 并退出 78。因此本轮所有用例以“stderr 不是 JSON / 退出码不是 2 或 0”失败；这是缺少 CLI 行为，而不是编译错误。等实现接入后，应按上表逐项转绿。

## 暂不能黑盒验证的场景与最小测试缝

以下场景需要整合者先同意一个可注入的依赖缝；本轮不写假断言，也不猜公开字段。

1. **研究生课表/教学日历解析**（`test_grad.py::test_parse_timetable_*`、`test_merge_periods_collapses_consecutive_blocks`、`test_parse_course_cell_*`、`test_parse_calendar_*`、`test_term_from_html_*`；`test_cli_errors.py::test_timetable_calendar_uses_the_page_term`、`test_timetable_calendar_without_term_raises`）
   - 需要可替换 `grad_browser.collect` 的页面来源（或明确的本地 fixture transport），把 `timetable_graduate.html` / `calendar_graduate.html` 喂给 `schedule update`。
   - 重点：rowspan 展开、节次时钟、合并连续节次、中文周次到 20、12 月/1 月跨年、缺 `dgData`/缺学期标签时报错不猜年份。
2. **研究生考试共享外壳**（`test_grad.py::test_parse_exams_returns_columns_even_when_empty`、`test_parse_exams_parses_rows`；合同“`exams` 按身份选数据源，输出共同外壳，不伪造考试日期、场次、地点”）
   - 需要同一页面注入缝，或直接约定 `exams` 的共享字段。当前只验证了旧别名被移除和角色守卫；空表/有行表的成功输出、按身份选源、不得编造字段仍待集成用例。
   - 夹具已备：`exams_graduate_empty.html`、`exams_graduate_rows.html`。
3. **本科生课表 SjdDay 分支**（`test_timetable.py::test_fetch_undergraduate_builds_dates_from_sjd_day`、`test_fetch_undergraduate_without_dates_raises`）
   - 需要移动教务响应注入缝。重点：日期取自 `SjdDay.mxrq` 而非算术推断；没有日期时报错。
4. **本科生 exams/scores/classrooms 成功输出**
   - 需要可注入的移动教务 HTTP 传输或录制响应。当前未联网，因此只覆盖身份拒绝和参数错误；`scores --semester` 过滤、考试空数据、classroom 空/非空解析尚未验证。
   - `test_cli_errors.py::test_undergraduate_role_is_not_blocked` 同样需要该缝才能验证“本科生不会被守卫挡住”。
5. **Chrome/CDP/临时 profile 生命周期**（`test_grad_browser.py` 全部、`test_cdp.py` 全部）
   - 需要可替换浏览器路径、可脚本化的 CDP 端点以及可替换临时目录。本轮明确不启动浏览器、不实现或测试安全控制绕过；这些应在实现阶段以单元测试或专门的本地假 Chrome 缝覆盖。
6. **课表保存权限/原子替换**（`test_timetable.py::test_save_and_load_round_trip` 中 `0600`）
   - 需要成功的 `schedule update` 注入缝，或实现侧的存储单元测试。当前只验证读取/拒绝时不会改写缓存。
7. **过期但归属明确的快照**
   - 合同没有明确“过期快照是直接返回、还是提示刷新”。本轮用围绕当天生成的教学周避开歧义；若实现选择提示刷新，应补一条明确的跨学期用例，而不是让测试依赖运行日期。
8. **本周/今天/明天的时钟边界**
   - 需要可注入时钟或固定 `--date` 已覆盖单日；跨周（周日/周一）、学期外 fallback 仍待确定语义后补。
9. **Windows 下打开导出目录失败**
   - 当前只在 Unix 伪造 `open` 失败；Windows 的 explorer 启动失败需在 Windows 验收或实现侧的启动器抽象中覆盖。
10. **课表/考试页面真实上游结构**
    - 属于校园网真实验收，按合同另行记录并经用户确认后运行；不得混入离线测试。
