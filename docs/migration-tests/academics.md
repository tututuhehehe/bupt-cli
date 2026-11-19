# 教务与课表迁移测试清单（academics）

Python 基准：`d7c35ef`。合同：`docs/rust-migration.md`。本清单只覆盖 timetable/schedule、exams、scores、classrooms 的公开行为；不搬运 Python 私有函数或内部数据结构。

## 测试文件与夹具

- `tests/contract_academics.rs`：全部黑盒 CLI 用例，使用共享 `mod contract_support;`。
- `tests/fixtures/contracts/academics/*.html`：脱敏、合成的研究生课表/教学日历/考试表格，供后续有注入缝时验证解析。当前没有被编译进测试，因为黑盒 CLI 还没有可注入页面来源的公开缝。
- 测试凭据、密码、账号、课程、教师、房间均为合成值；不读取真实 `~/.bupt-cli`、浏览器 profile 或校园服务。

## 关键约定

1. **课表快照归属与当前学期**：Rust 快照格式为 v2，顶层带 `username` 和 `role`。只有 `username` 等于当前凭据、`role` 等于当前身份、且运行当天落在快照教学周日期的闭区间内，才可直接服务 `schedule` / `schedule calendar` / `schedule export`。v1 没有 `username`，属于“无法确认归属”，不能按 `role` 猜测后继续。归属不符、缺失、损坏或当天不在学期内的快照都走唯一一次自动刷新；刷新失败/不完整/不覆盖当天时保留旧文件但不把它当作当前学期，也不生成日历或导出。
2. **缓存位置**：沿用旧行为，fixture 写到 `BUPT_CLI_CACHE_DIR/timetable.json`；凭据写到 `BUPT_CLI_CONFIG_DIR/credentials.json`。
3. **网络隔离边界**：共享 `Sandbox` 只隔离 HOME/config/cache。把 `HTTP(S)_PROXY`/`ALL_PROXY` 指向关闭的本地端口是 best-effort，只对遵守这些环境变量的 HTTP 客户端生效；它不覆盖浏览器/非 HTTP 流量，也不能证明“没有发起网络请求”。真正的 no-calls 断言必须放在后续注入 transport/browser 的场景里（见下）。
4. **动态日期与时钟**：进程用例的教学周围绕本机当天生成（前后各数周），避免测试在学期结束后因“过期”而误判；实现侧用例使用固定日期 `2026-09-23`，并显式把 `today` 传给 `select_snapshot`。运行时使用本机日期，无法确定本机日期则报告错误。
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
| `corrupt_and_wrong_version_caches_require_an_explicit_update` | `test_timetable.py::test_load_rejects_corrupt_or_wrong_version` | 损坏 JSON、未来版本、缺 `courses` 的快照不导致 panic/假成功；自动刷新不可用时显式报告，不重写缓存 |
| `unattributed_v1_cache_is_refused_instead_of_guessed` | 合同“不能确认旧快照归属时提示刷新” | v1 有 `role` 但无账号：query/calendar/export 全部拒绝，提示刷新，不生成导出文件 |
| `cache_owned_by_another_account_is_refused` | 合同“只读当前学号及身份的可用课表快照” | v2 快照属于另一学号：三种读取命令全部拒绝，不泄漏/导出旧账号课表 |
| `cache_for_another_role_is_refused` | 合同同上；`test_cli_errors.py` 角色守卫 | v2 快照 role 与当前身份不符：三种读取命令全部拒绝，不泄漏旧身份课表 |
| `missing_snapshot_reports_unavailable_refresh_without_fabricating_cache` | 合同“上游不可用时显式失败，不伪造数据” | 没有快照时自动刷新只尝试临时来源一次；退出 5，错误说明“没有接通/不会伪造”，不创建缓存 |
| `manual_update_without_a_source_preserves_the_existing_snapshot` | 合同“刷新失败保留旧快照” | `schedule update` 强制刷新，但未接来源：退出 5，旧快照字节不变，无临时文件 |
| `out_of_range_week_points_at_explicit_update_and_keeps_the_snapshot` | `commands.cmd_schedule` 中 `--week` 不存在的分支 | 合法整数但不在快照范围：退出 2，hint 指向 `schedule update`，缓存不变 |
| `invalid_weekday_entries_are_ignored_not_wrapped_or_fabricated` | `test_timetable.py::test_occurrences_ignores_out_of_range_weekdays` | 手改缓存中的 `-1`/`7`/`99` 不崩溃、不绕到周日、不伪造课程；返回 count 0 |
| `graduate_accounts_are_refused_by_undergraduate_jwgl_commands` | `test_cli_errors.py::test_undergraduate_jwgl_commands_refuse_graduate_accounts` | `scores`/`classrooms` 对研究生身份在联网前退出 2，说明走本科教务；不生成 session/cache |
| `role_guards_only_refuse_graduate_accounts_not_unknown_passwords` | `test_cli_errors.py::test_undergraduate_role_is_not_blocked` | 缺密码时 `exams`（研究生）/`scores`（本科生）都停在 auth，而不是被本科教务守卫误拒；不写 session/cache |
| `unsupported_role_is_rejected_before_backend_selection` | 合同“登记显式选择的本科/研究生身份” | 未知 role 不被猜测为本科/研究生，命令在选后端前退出 2；不触碰缓存/session |

## 实现侧刷新边界用例

这些用例在 `src/academics.rs` 的私有 `#[cfg(test)]` 模块里，通过 `select_snapshot(..., today, ..., &mut FakeSource)` 直接注入日期和假的课表来源；不启动进程、不联网、不依赖真实时钟。

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
| `atomic_writes_leave_predictable_temp_files_untouched` | 普通 `timetable.json.tmp` / `output.ics.tmp` 预置内容在写入后仍逐字节不变；实际写入使用随机 `NamedTempFile` |
| `atomic_writes_do_not_follow_planted_temp_symlinks` | 预置临时路径若是指向无关文件的 symlink：保存快照和导出都不跟随，victim 内容与链接目标都不变 |
| `atomic_write_refuses_a_symlink_target_without_touching_it` | 目标本身是 symlink 时拒绝替换；返回 upstream 错误，链接和指向文件都不变 |
| `concurrent_atomic_writes_leave_one_complete_file` | 多个并发写同一目标：最终内容是某一次完整 payload，不出现交叉/截断内容 |

## 实施状态（本地当前学期与完整导出）

- `src/academics.rs` 已实现一个共享的本地优先选择/刷新边界：`schedule`、`schedule calendar`、`schedule export` 在读取前先验证 v2 归属与运行当天是否在学期闭区间内；缺失、过期、归属不符或损坏时最多自动取数一次。`schedule update` 复用同一边界并强制刷新。生产来源 `UnavailableSource` 明确返回“当前构建没有接通教务课表刷新；不会伪造课表数据”（退出 5），不触碰网络或浏览器。
- 取数成功后才替换 `timetable.json`；刷新失败、数据不完整或不覆盖当天时保留旧文件，但拒绝把旧文件当作当前学期。取数结果还会校验每个教学周恰好 7 个连续合法日期；`term_bounds` 不再独自承担完整性判断，因此“零课程 + 畸形/空日历”也不会被部分写回。
- 缓存与 `.ics` 共用同一个原子写入助手：在目标同目录用 `NamedTempFile` 创建不可预测临时文件，写满、flush、`sync_all` 后才 `persist`。目标若是 symlink、非普通文件或与临时文件同一 inode 会拒绝；预置的可预测 `.tmp` 文件/链接不会被跟随或截断，写入失败保留旧目标。`tempfile` 从 dev-dependency 移到正式 dependency，未增加 crate，`Cargo.lock` 无变化。
- `schedule export` 先对全部场次做完整校验：日期必须来自教学周日期表，时间支持 `8:00`、`08:00`、`0800`、全角冒号等可识别形式并规范化；任一课程缺日期/时间或时钟非法时点名课程并整次失败。教师/教室缺失允许。校验通过后才走上述原子写入，失败不会覆盖已有 `.ics`。
- `scores` / `classrooms` / `exams` 仍只完成本地身份与数据源路由校验，之后返回 upstream pending（退出 5）；没有成功输出、网络、浏览器或真实校园数据。
- 本清单的进程用例 30 个、实现侧刷新边界/安全写入用例 13 个全部通过；没有用跳过或假断言绕过尚未接通的远端路径。`cargo fmt --check`、`cargo test --locked --offline --lib`、`cargo test --locked --offline --test contract_academics` 是交付检查项。
- 本地凭据统一由 `identity::require_authenticated_profile` 读取 `credentials.json`：username 和非空密码是登录前提；`role` 只在需要选择/校验数据源的 `schedule`、`exams`、`scores`、`classrooms` 中要求。`BUPT_USERNAME` / `BUPT_PASSWORD` / `BUPT_ROLE` 不再读取，也不再能补足或覆盖 JSON。
- 当前学期策略：只看运行当天是否在快照日期闭区间内，不看 `--date`、不看 TTL、不按抓取时间淘汰；学期内不会因快照“旧”而自动刷新。跨学期时按上述边界自动刷新一次。

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
6. **课表保存权限**（`test_timetable.py::test_save_and_load_round_trip` 中 `0600`）
   - 当前刷新写缓存与导出写 `.ics` 已统一走同目录 `NamedTempFile` + `sync_all` + `persist`，并拒绝 symlink 目标/同 inode 别名；失败保留旧目标。尚未验证 Unix `0600` 权限，也未在 Windows 实测覆盖已有目标的持久化行为；需要平台验收。
7. **本周/今天/明天的本地日与时钟边界**
   - 整合版通过现有 `time` 依赖使用本机日期，和 Python `date.today()` 对齐。选择函数仍可注入日期；`DTSTAMP` 保持 UTC。Windows 实机和跨时区仍需独立验收。
8. **Windows 下打开导出目录失败**
   - 当前 Unix 用 sentinel 证明伪造 `open` 被调用且退出 9 后导出仍成功；Windows 的 explorer 路径与启动失败仍未实测，需在 Windows 验收或实现侧的启动器抽象中覆盖。
9. **课表/考试页面真实上游结构**
   - 属于校园网真实验收，按合同另行记录并经用户确认后运行；不得混入离线测试。
