# 教务与课表迁移测试清单（academics）

Python 基准：`d7c35ef`。合同：`docs/rust-migration.md`。本清单只覆盖 timetable/schedule、exams、scores、classrooms 的公开行为；不搬运 Python 私有函数或内部数据结构。

## 测试文件与夹具

- `tests/contract_academics.rs`：全部黑盒 CLI 用例，使用共享 `mod contract_support;`。
- `tests/fixtures/contracts/academics/*.html`：脱敏、合成的研究生课表/教学日历/考试表格，供后续有注入缝时验证解析。当前没有被编译进测试，因为黑盒 CLI 还没有可注入页面来源的公开缝。
- 测试凭据、密码、账号、课程、教师、房间均为合成值；不读取真实 `~/.bupt-cli`、浏览器 profile 或校园服务。

## 关键约定

1. **课表快照归属**：用例假设 Rust 快照格式为 v2，顶层带 `username` 和既有 `role`。只有 `username` 等于当前凭据、`role` 等于当前身份时才可服务 `schedule` / `schedule calendar` / `schedule export`。v1 没有 `username`，属于“无法确认归属”，必须提示 `schedule update`，不能按 `role` 猜测后继续。
2. **缓存位置**：沿用旧行为，fixture 写到 `BUPT_CLI_CACHE_DIR/timetable.json`；凭据写到 `BUPT_CLI_CONFIG_DIR/credentials.json`。
3. **网络隔离边界**：共享 `Sandbox` 只隔离 HOME/config/cache。把 `HTTP(S)_PROXY`/`ALL_PROXY` 指向关闭的本地端口是 best-effort，只对遵守这些环境变量的 HTTP 客户端生效；它不覆盖浏览器/非 HTTP 流量，也不能证明“没有发起网络请求”。真正的 no-calls 断言必须放在后续注入 transport/browser 的场景里（见下）。
4. **动态日期**：快照的教学周围绕 UTC 当天生成（前后各数周），避免测试在学期结束后因“过期”而误判；断言的具体日期由同一套日期算法算出。
5. **凭据来源**：只用临时目录里的 `credentials.json`；账号环境变量不读取、不覆盖 JSON。role 只用于需要身份路由的教务操作。

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
| `passwordless_profile_cannot_read_a_matching_snapshot` | 合同“`logout` 清掉本地密码和会话，保留课表”；身份/快照归属 | 密码被清掉但 username/role 仍匹配时，query/calendar/export 全部 auth 拒绝；快照字节不变，不生成 ICS |
| `environment_credentials_do_not_fill_a_passwordless_profile` | 旧凭据优先级的有意变更 | 配置缺密码时，即使合成 `BUPT_USERNAME` / `BUPT_PASSWORD` / `BUPT_ROLE` 全部存在，也不能补足本地 profile；查询停在 auth，快照不变 |
| `environment_credentials_do_not_override_saved_account_or_role` | 旧凭据优先级的有意变更 | 有效 JSON 的学号和 role 优先于冲突的账号环境变量；匹配的 graduate 快照仍可读取，role 输出保持 `graduate` |
| `date_arguments_require_ascii_yyyy_mm_dd_and_valid_year` | 合同“非法参数在读写业务状态前拒绝” | 带符号年份/分量、0000 年、非 ASCII 数字、非补零月日、非法闰日全部 usage；不触碰缓存 |
| `valid_leap_day_is_accepted_by_the_local_query` | `--date` 合法日期边界 | 2024-02-29 被接受并返回空结果；快照不变 |
| `export_omits_events_with_invalid_clock_times` | 合同“不伪造考试/课表事实”；安全 ICS | 25:00、09:60 等非法时钟在插值前被拒绝，不生成对应事件，事件计数只算真实输出 |
| `export_counts_real_events_and_escapes_injected_line_breaks` | 安全 ICS 导出 | 课程名/房间中的 CRLF、裸 LF、`BEGIN:VEVENT` / `END:VEVENT` 被转义成文本；物理事件边界仍各只有一条，`events` 不按子串计数 |
| `corrupt_and_wrong_version_caches_require_an_explicit_update` | `test_timetable.py::test_load_rejects_corrupt_or_wrong_version` | 损坏 JSON、未来版本、缺 `courses` 的快照不导致 panic/假成功；提示 `schedule update`；不重写缓存 |
| `unattributed_v1_cache_is_refused_instead_of_guessed` | 合同“不能确认旧快照归属时提示刷新” | v1 有 `role` 但无账号：query/calendar/export 全部拒绝，提示刷新，不生成导出文件 |
| `cache_owned_by_another_account_is_refused` | 合同“只读当前学号及身份的可用课表快照” | v2 快照属于另一学号：三种读取命令全部拒绝，不泄漏/导出旧账号课表 |
| `cache_for_another_role_is_refused` | 合同同上；`test_cli_errors.py` 角色守卫 | v2 快照 role 与当前身份不符：三种读取命令全部拒绝 |
| `out_of_range_week_points_at_explicit_update_and_keeps_the_snapshot` | `commands.cmd_schedule` 中 `--week` 不存在的分支 | 合法整数但不在快照范围：退出 2，hint 指向 `schedule update`，缓存不变 |
| `invalid_weekday_entries_are_ignored_not_wrapped_or_fabricated` | `test_timetable.py::test_occurrences_ignores_out_of_range_weekdays` | 手改缓存中的 `-1`/`7`/`99` 不崩溃、不绕到周日、不伪造课程；返回 count 0 |
| `graduate_accounts_are_refused_by_undergraduate_jwgl_commands` | `test_cli_errors.py::test_undergraduate_jwgl_commands_refuse_graduate_accounts` | `scores`/`classrooms` 对研究生身份在联网前退出 2，说明走本科教务；不生成 session/cache |
| `role_guards_only_refuse_graduate_accounts_not_unknown_passwords` | `test_cli_errors.py::test_undergraduate_role_is_not_blocked` | 缺密码时 `exams`（研究生）/`scores`（本科生）都停在 auth，而不是被本科教务守卫误拒；不写 session/cache |
| `unsupported_role_is_rejected_before_backend_selection` | 合同“登记显式选择的本科/研究生身份” | 未知 role 不被猜测为本科/研究生，命令在选后端前退出 2；不触碰缓存/session |

## 实施状态（本地第一批）

- `src/academics.rs` 已实现本地部分：v2 快照归属校验、可用当前 profile 校验（密码被 logout 清掉的配置不能单独读取课表/导出）、`schedule` 单日/周/原始查询、`schedule calendar`、`schedule export`（含严格日期/HH:MM 校验、ICS 折行、CRLF/换行转义和 opener 失败隔离），以及 `scores` / `classrooms` / `exams` 的本地身份与数据源路由校验。
- 本清单 25 个进程用例全部通过；没有用跳过或假断言绕过尚未接通的远端路径。
- `schedule update`、本科生 `scores` / `classrooms` / `exams` 和研究生 `exams` 在通过本地校验后返回 upstream pending（退出 5），不访问 transport、校园网或浏览器。对应的成功输出场景仍在下面的暂缓清单中，尚未编译成用例，因此当前也不会以红测出现。
- 本地凭据统一由 `identity::require_authenticated_profile` 读取 `credentials.json`：username 和非空密码是登录前提；`role` 只在需要选择/校验数据源的 `schedule`、`exams`、`scores`、`classrooms` 中要求。`BUPT_USERNAME` / `BUPT_PASSWORD` / `BUPT_ROLE` 不再读取，也不再能补足或覆盖 JSON。
- 当前过期策略：归属明确且版本兼容的 v2 快照即使已离开教学周，仍会按本地快照返回；不会自动刷新。合同没有明确“过期是提示刷新还是继续读快照”，这里先选择严格离线、显式刷新，需整合者确认后再补跨学期用例。

## 暂不能黑盒验证的场景与最小测试缝

以下场景需要整合者先同意一个可注入的依赖缝；本轮不写假断言，也不猜公开字段。

**注入缝的硬性断言**：`schedule` / `schedule calendar` / `schedule export` 的成功与拒绝路径都必须断言 transport/browser 调用日志为 0 次；`schedule update`、`exams` 等显式取数路径则断言目标与调用次数。仅把代理指向关闭端口不满足这一要求，也不能代替调用日志。

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
8. **本周/今天/明天的本地日与时钟边界**
   - 旧 Python 用 `date.today()`（用户本地日）；当前 Rust 只用 `std::time`，没有本地时区 API，因此 today/tomorrow/week 暂用 UTC 日，DTSTAMP 按合同保持 UTC。这是明确的未决差异，不冒充已修复。
   - 具体依赖请求：由整合者批准并加入共享依赖 `time`（启用 `local-offset`）或 `jiff`，或提供可注入的共享 clock；在此之前不修改本线 Cargo。跨周（周日/周一）、学期外 fallback 仍需该 seam 后补。
9. **Windows 下打开导出目录失败**
   - 当前 Unix 用 sentinel 证明伪造 `open` 被调用且退出 9 后导出仍成功；Windows 的 explorer 路径与启动失败仍未实测，需在 Windows 验收或实现侧的启动器抽象中覆盖。
10. **课表/考试页面真实上游结构**
    - 属于校园网真实验收，按合同另行记录并经用户确认后运行；不得混入离线测试。
