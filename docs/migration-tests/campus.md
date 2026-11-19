# campus lane：测试迁移清单

范围：`library search|hold|ebook`、`electricity`、`electricity buildings`、`notices`、`notices show`、`card`。
Python 基准：`d7c35ef` 的 `tests/test_library.py`、`tests/test_electricity.py`、`src/bupt_cli/{library,commands,config}.py`，以及 `src/bupt_cli/_vendor/campus_api/bupt/{portal,electricity,models,constants}.py`。
Rust 合同：`docs/rust-migration.md`。本文件记录测试、实现边界与未决项；合同冲突时以 `docs/rust-migration.md` 为准。

## 已执行的离线用例

`tests/contract_campus.rs` 全部通过公开 CLI 进程边界验证，使用共享 `Sandbox`，只用合成输入，不读取真实凭据、不依赖校园网响应。所有错误用例都断言：stdout 为空、stderr 是单个 JSON、带 `schema: "bupt.v1"`、`ok: false`、正确的 `error.type` 和退出码。进程级用例没有成功响应注入，只能区分本地参数、配置和凭据层，不能证明实现没有发起请求；库解析、请求形状、门户/电费完整请求链由模块内的 `Scripted` 会话驱动测试覆盖。

| Rust 用例 | 覆盖语义 | Python 来源 |
|---|---|---|
| `library_search_rejects_blank_keyword_without_reading_credentials` | 空白/空关键词在凭据解析前以 usage 失败；损坏的 credentials 不会抢先成为 auth | `test_search_rejects_blank_keyword`，`library.py:search` |
| `library_hold_and_ebook_reject_blank_ids_without_reading_credentials` | hold/ebook 空 id 在凭据读取前以 usage 失败；错误点名 `recCtrlId` | `test_holdings_rejects_blank_id`、`test_electronic_resources_rejects_blank_id` |
| `library_requires_explicit_subcommands_and_arguments` | `library`、`search`、`hold`、`ebook` 缺子命令/位置参数均为 usage | `cli.py` library 分派 |
| `library_rejects_invalid_pagination` | `--page` / `--page-size` 下界、非整数、`--timeout 0` 在参数解析阶段拒绝 | `cli.py:_int_at_least`、`_positive_float` |
| `electricity_default_placeholder_needs_a_campus_session` | 无参数使用占位宿舍后进入凭据层，返回 auth 3；不创建会话缓存 | `test_default_dorm_is_xitucheng_xue1_101` 的本地部分 |
| `electricity_building_flag_requires_a_room_without_falling_back` | 传 `--building` 时即使 env/config 有 room 也要求 `--room` | `test_building_without_room_is_usage_error`、`resolve_dorm` |
| `electricity_rejects_unknown_building` | 未知楼名是 usage，提示指向 `electricity buildings` | `test_unknown_building_is_usage_error` |
| `electricity_accepts_documented_building_aliases_before_auth` | 学五楼/学5楼/学5/5/5楼/学十一楼/学29楼/SHAHE_A/shahe_a/沙河校区雁北园A楼均可本地解析并进入凭据层 | `test_building_aliases` |
| `electricity_environment_building_wins_over_config` | env building 覆盖 config，且不会因 config 楼名无效而失败 | `test_env_beats_config` |
| `electricity_flag_building_wins_over_environment` | flag building 覆盖 env | `test_flags_beat_env_and_config` 的楼名分支 |
| `electricity_config_building_is_used_when_flags_and_env_are_absent` | 无 flag/env 时读取 config：有效楼名返回 auth；把 config 楼名改为无效值后必须 usage 并回显输入，证明不是忽略 config 而用了默认值 | `test_config_used_when_no_flag_or_env` |
| `electricity_rejects_legacy_area_on_the_query_command` | 顶层 `electricity --area` 是 usage，提示改用 `electricity buildings`；损坏 config 没有抢先 | `test_area_is_rejected_on_top_level_electricity` |
| `electricity_buildings_rejects_unknown_area` | `buildings --area mars` 在缺失凭据/损坏配置前给出 usage | `test_buildings_subcommand_rejects_bad_area` |
| `electricity_buildings_ignores_malformed_dorm_config` | 损坏的 `config.json` + 缺失凭据时返回 auth 并提示登录，而不是 config usage；只证明配置失败没有抢先，不证明请求次数 | `test_buildings_subcommand_ignores_broken_config` |
| `electricity_buildings_accepts_documented_area_aliases` | shahe/沙河/2/xitucheng/haidian/西土城/海淀/1 均本地解析并到达凭据层；不证明请求次数 | `_resolve_area`、`test_buildings_subcommand_accepts_xitucheng` |
| `electricity_rejects_invalid_thresholds` | 负值、NaN、inf、非数字为 usage | `cli.py:_float_at_least` |
| `electricity_accepts_zero_threshold_before_auth` | 阈值 0 合法，继续到凭据层 | `test_default_dorm_query_shape` 的 threshold 边界 |
| `electricity_reports_malformed_config_as_usage_error` | 损坏 JSON 是 usage，hint 提到 JSON；不断言具体文案或底层 IO 措辞 | `test_bad_config_json_is_usage_error` |
| `electricity_rejects_non_object_config_and_dorm_sections` | 顶层非对象、`dorm` 非对象均为 usage | `test_non_object_dorm_config_is_usage_error` |
| `electricity_reports_unreadable_config_without_json_hint` | 路径是目录时是 usage，且消息/hint 不含 JSON；不断言权限或 IO 文案 | `test_unreadable_config_is_usage_error_not_json_hint` |
| `notices_requires_credentials_without_creating_session` | 无凭据时 notices 返回 auth 3，提示登录；不创建会话文件（不等于证明无请求） | `commands.py:cmd_notices_list` |
| `card_requires_credentials_without_creating_session` | 无凭据时 card 返回 auth 3，提示登录；不创建会话文件（不等于证明无请求） | `commands.py:cmd_card` |
| `environment_only_credentials_are_ignored` | 只有合成账号环境变量、没有 `credentials.json` 时 card 仍为 auth 3；`BUPT_USERNAME` / `BUPT_PASSWORD` / `BUPT_ROLE` 不再是登录来源 | 合同“已登记凭据只从 credentials.json 读取” |
| `malformed_saved_credentials_are_an_auth_error` | 损坏的 `credentials.json` 报 auth 3 并提示登录，不回显文件内容，不创建会话 | `credentials.py` 的 auth 错误分类；合同凭据边界 |
| `saved_credentials_do_not_require_a_role` | 有效 JSON 只有 username + password、没有 role 时 card 仍进入 session adapter pending（upstream 5）；campus 不要求 role | 合同“role 只在需要身份路由时要求” |
| `notices_rejects_unknown_types_before_auth` | 未知/混合类型在凭据前 usage，hint 含 `notice` 类别 token；尾随逗号按旧版忽略，不在此列 | `commands.py:_notice_types` |
| `notices_accepts_case_insensitive_types_before_auth` | 类型大小写不敏感，合法类型继续到凭据层 | `_notice_types`、`NoticeType` |
| `notices_rejects_invalid_count_as_usage_error` | `--count` 负数/非整数/小数在凭据前 usage | `test_invalid_numeric_arguments_are_usage_errors` |
| `notices_show_requires_a_url_and_non_negative_max_chars` | 缺 URL、负 `--max-chars` 为 usage | `cli.py` notices show 参数 |
| `notices_show_rejects_blank_url_before_auth` | 空白 URL 在无凭据时仍为 usage 2，证明校验先于 profile/auth；提示指向 `bupt notices` | 本轮评审决定，覆盖旧版空 URL 落 upstream 5 的行为 |
| `notices_show_requires_credentials_without_creating_session` | 有效 URL 但无凭据时 auth 3 并提示登录；使用 `.invalid` 合成 URL；不创建会话文件（不等于证明无请求） | `commands.py:cmd_notices_show` |
| `notices_and_card_are_not_blocked_by_malformed_dorm_config` | 损坏 dorm config + 缺失凭据时 auth 并提示登录，config usage 没有抢先 | `commands.py` portal 分派 |
| `card_rejects_unexpected_arguments` | card 不收 `--area`/位置参数 | `cli.py` card 参数 |

### 边界与副作用断言

- 所有 usage/auth 用例都断言没有 `cache/session.json` 新文件。这只是没有落盘会话的证据，不是没有发起请求的证据。
- library 的空白输入用例故意放一个损坏的 `credentials.json`；library 仍报 usage，证明匿名校验不会解析账号配置并转成 auth。
- `electricity buildings` 用例故意放损坏的 `config.json` 并保持无凭据；返回 auth 且提示登录，说明 config usage 没有抢先。错误文案不作断言，也不能证明没有请求。
- 缺 room、顶层 `--area` 用例故意放损坏配置；报具体 flag/参数错误，证明这些参数校验先于业务配置读取。未知楼名在 Python 中位于配置读取之后，未用损坏配置做顺序断言。
- 所有断言只使用合成输入：楼名是公开常量或占位符，URL 使用 `example.invalid`，没有真实学号、密码、Cookie 或 token；测试不依赖校园网响应。进程级用例仍不能证明实现没有发请求；内部 `Scripted` 测试只回放合成响应。

## 合成夹具

`tests/fixtures/contracts/campus/` 是脱敏的上游响应形状；`src/campus/library.rs`、`src/campus/portal.rs` 与 `src/campus/electricity.rs` 的内部 `Scripted` 会话/传输测试直接消费图书馆、门户和电费响应，不联网。

| 夹具 | 预期用途 |
|---|---|
| `library_search.json` / `library_search_empty.json` | 书目结果字段映射、高亮标签/实体清理、坏计数为 null、空结果 |
| `library_holdings.html` | 馆藏行：固定状态列、列偏移回退、非馆藏行过滤、可借计数 |
| `library_ebook.json` / `library_ebook_empty.json` | 扁平 `{locate, recNote}`、`\x1e` 清理、空列表 `has_electronic=false` |
| `portal_notices_notice.html` | 通知列表：相对 URL 补全、author/time、空 href 过滤；`src/campus/portal.rs` 已消费 |
| `portal_notices_news.html` | 新闻列表；命令级默认 `notice,news` 双请求与合并排序测试消费 |
| `portal_notice_detail.html` | 详情：标题、发布部门/时间、正文、相对图片、装饰图过滤、附件；`src/campus/portal.rs` 已消费 |
| `portal_card.json` / `portal_card_invalid.txt` | 卡余额字段映射；非 JSON 错误；`src/campus/portal.rs` 已消费 |
| `portal_login_page.html` | 门户登录页识别（auth 分类，而不是 parse/network）；列表与卡余额均已消费 |
| `electricity_buildings_shahe.json`、`electricity_buildings_empty.json` | 楼栋列表字段原样映射、空列表 `count=0` |
| `electricity_floors.json`、`electricity_floors_xitucheng.json`、`electricity_rooms.json` | 沙河/西土城楼层形状、房间匹配与 `dromNum`；`src/campus/electricity.rs` 的查询链测试已消费 |
| `electricity_meter.json`、`electricity_meter_empty.json` | 电表余额字段映射；空/零值的默认值、`freeEnd` 回退 |
| `electricity_auth_error.json` | 上游 `e/m` 未登录标记 |
| `electricity_upstream_error.json` | `e != 0` 且无登录标记的维护错误 |

## 命令编排与会话边界

`src/campus.rs` 保留命令注册、参数校验和路由，并新增 `run_with_session`；`src/campus/session.rs` 定义 `SessionDriver` 与 `Route`；`src/campus/library.rs` 负责匿名 OPAC；`src/campus/dorm.rs` 负责宿舍配置、楼名和楼栋元数据；`src/campus/portal.rs` 负责门户请求/响应；`src/campus/electricity.rs` 负责电费完整请求链和字段映射。已实现：

- `library search|hold|ebook` 的参数规格、匿名 OPAC 请求和响应解析。匿名 `library` 路径不读取凭据，也不使用 `SessionDriver`。
- `electricity` / `electricity buildings` 的参数规格、楼名与校区别名、`config.json` 的 `dorm` 解析，以及 flag > env > config > built-in placeholder 的优先级。
- `electricity buildings` 的 `/part` 请求和 `area`/`count`/原样列表映射。
- `electricity` 余额的 Python 请求链：静态楼栋 `partmentId`/`areaId`/默认楼层 → `/floor` 精确楼层名匹配 → `/drom` 房间名/房号评分 → `/search` → CLI 字段。楼层推断、沙河 `层` 后缀、房间规范化/精确>后缀>包含、非对象项、缺 `dromNum` 都有离线用例。
- `notices` / `notices show` / `card` 的本地参数校验、profile 检查和命令编排；`notices show` 的空白/外部 URL 在读取 profile 前以 usage 2 拒绝。受保护命令统一使用 `identity::require_authenticated_profile`，只从临时目录的 `credentials.json` 读取 username 和非空密码，不读取账号环境变量，也不要求 role；无本地 profile 或凭据损坏时返回 auth 3。
- `SessionDriver` 在联网前调用 `ensure(Service::Portal|Electricity)`，每个请求带 `Route` 标签；`send` 只执行一次交换，`renew` 只在确认 auth 3 后调用一次。门户列表 auth 后重启整个列表；电费只重试失败的 `/post`。这与 Python 的 invalidate-and-retry 行为一致。`Route::accepts` 固定方法、完整 URL/主机、请求头集合和空/非空 body，驱动不得把 `Cookie`/`Authorization` 放进 `Request`。
- 默认 `run` 使用 `PendingSession`，在本地参数和 profile 检查后返回 upstream 5；因此当前正式二进制仍不会发门户/电费请求。父会话层可调用 `run_with_session` 接入真实 `ensure`/`send`/`renew`。

`SessionDriver` 收到的 route 模板（`Route::accepts` 逐项校验）：

| Route | 方法 | 目标与固定请求头 |
|---|---|---|
| `PortalNoticeList` | GET | `http://my.bupt.edu.cn/list.jsp?...&wbtreeid=<notice/news/rules/ways>`；无自定义头、无 body |
| `PortalNoticeDetail` | GET | 用户给定、已校验的 `http(s)://my.bupt.edu.cn/...`；无自定义头、无 body |
| `PortalCard` | GET | `http://my.bupt.edu.cn/system/resource/app/cuser/getwxtsA.jsp`；`Referer` + `X-Requested-With` |
| `ElectricityBuildings` | POST | `https://app.bupt.edu.cn/buptdf/wap/default/part`；`Referer` + `X-Requested-With` + `Content-Type`，非空 form body |
| `ElectricityFloors` | POST | `.../floor`；同上 |
| `ElectricityRooms` | POST | `.../drom`；同上 |
| `ElectricityBalance` | POST | `.../search`；同上 |

任何 `Cookie`/`Authorization` 头、其他 URL/方法、空/非空 body 不符都会在驱动前被拒绝。门户列表和卡控件固定为 Python 的 `http`；详情允许 `http`/`https` 但主机必须是 `my.bupt.edu.cn`。

### 本轮新增的内部 fixture 测试

这些测试在 `src/campus/library.rs` / `src/campus/dorm.rs` / `src/campus/electricity.rs` 的 `#[cfg(test)]` 中；图书馆和电费映射测试通过共享 `Scripted` 传输回放合成夹具，不联网。门户测试见下一节：

| Rust 测试 | 覆盖 |
|---|---|
| `clean_text_strips_highlight_and_decodes_entities` | `<b>` 清理、HTML 实体、空白折叠 |
| `library_search_parses_fixture_and_sends_required_fields` | 书目字段/坏计数 null、`solrType=TITLE`、`xiaoqu`、分页表单字段、POST 请求头 |
| `library_search_parses_empty_fixture` | `total=0`、`books=[]` |
| `library_hold_parses_status_columns_and_campus_rows` | 状态列、列偏移回退、非馆藏行过滤、可借计数、GET 请求头 |
| `library_ebook_parses_flat_resources_and_empty_state` | 两条资源、`\x1e` 去除、空列表、详情 Referer |
| `library_search_with_ebook_marks_failures_as_null` | 一次搜索 + 两次旁路，失败项为 `null` |
| `library_ids_are_escaped_as_single_path_segment` | hold URL 与 ebook Referer 把非 ASCII/保留字符 ID 编码为单一路径段；JSON `id` 与表单 `sid` 保持原值；简单字母数字不变 |
| `library_transport_errors_do_not_echo_internal_diagnostics` | 传输错误仍按 Python 分类映射为 usage 2；消息不含内部诊断，hint 保留 `diagnose` |
| `dorm_config_defaults_and_config_values_are_resolved_locally` | 内置占位符、config 值、`source=config` |
| `dorm_flags_win_over_environment_and_config` | flag > env > config、楼名/room/floor |
| `building_metadata_matches_the_python_constants` | 学一楼/沙河A/学29楼的 `partmentId`、`areaId`、默认楼层（含 `-1`、`层`、`强电`、空列表） |
| `buildings_maps_area_count_and_response_items` | `/part` 的 `areaid` 请求、shahe/xitucheng 回显、空列表、楼栋字段原样保留 |
| `balance_maps_fixture_fields_and_response_overrides` | `/search` 表单、响应 `parName/floorName` 覆盖 fallback、余额/价格/剩余量/总用量/时间/阈值/source 映射 |
| `balance_keeps_empty_and_zero_values_compatible` | 空 surplus、零/空 price、空 `vTotal` 回退 `freeEnd`、空名称/时间、低余额阈值 |
| `confirmed_login_markers_are_auth_errors` | 四个确认登录标记在 `e != 0` 时映射 auth 3，不把普通错误猜成 auth |
| `nonzero_status_without_auth_marker_keeps_python_classification` | 维护错误为 network 4；`请登录` 近似文本不升级为 auth；`无数据` 为 not_found 5 |
| `malformed_payloads_are_parse_errors` | 非对象、缺 `e`、缺 `d.data`、错误数据类型均为 parse 5 |
| `invalid_json_is_a_parse_error` | 非 JSON 响应为 parse 5 |
| `floor_inference_and_explicit_suffix_match_python_examples` | 学一楼 `101/1204` 首层回退、显式楼层 trim；沙河默认楼层与 `层` 后缀、显式 `强电` 原样；学29楼空默认楼层回退 `1` |
| `room_scores_are_exact_then_suffix_then_containment` | 房间名/房号规范化后 exact 3、后缀 2、包含 1、空/不匹配 0 |
| `query_rejects_a_floor_name_that_is_not_exactly_present` | `/floor` 名称不做模糊匹配；找不到时 not_found 5，只发一次请求 |
| `query_rejects_a_room_without_a_match_after_floor_and_room_requests` | `/drom` 无匹配时 not_found 5，请求链停在 `/drom` |
| `query_fails_honestly_on_non_object_floor_or_room_items` | 非对象楼层/房间项均为 parse 5，不 panic |
| `query_rejects_a_room_without_a_meter_number` | 缺少/空 `dromNum` 为 parse 5，不伪造电表号 |

### 门户响应核心（portal.rs）

`src/campus/portal.rs` 按 Python `portal.py` 的请求形状和 CLI 输出字段实现，测试全部通过共享 `Scripted` 传输回放合成夹具，不联网、不读取真实凭据：

| Rust 测试 | 覆盖 |
|---|---|
| `notice_types_resolve_defaults_case_insensitively_and_reject_unknown` | 无/空/仅逗号 → 默认类；大小写不敏感；未知类 usage 2，hint 列出四类 |
| `notices_fetch_default_types_and_sort_by_time_descending` | 默认 `notice,news` 两次 GET 列表 URL；跨类合并后按 `time` 倒序；`title/url/author/time/type/type_name`；相对 href 补全为门户绝对 URL；`types` 为 null；列表请求无自定义头、无 body |
| `notices_truncate_after_sorting_and_echo_requested_types` | 先排序再按 `--count` 截断 |
| `notices_zero_count_returns_empty_without_requests` | `--count 0` 返回 `count=0`、`notices=[]`，且零请求；显式 `types` 原样回显 |
| `notices_accepts_an_empty_newslist_but_rejects_listless_pages` | 有 `ul.newslist` 但无 `li` → 空成功；缺 `newslist` → parse 5；条目全无效 → parse 5 |
| `notice_list_classifies_login_and_system_error_pages` | 200 登录页 → auth 3（hint 指向 login）；200 系统错误页 → parse 5 |
| `portal_status_codes_map_to_auth_upstream_and_network_without_diagnostics` | 302/401/403 → auth 3 且不跟随；404/500 → upstream 5；传输错误 → network 4，消息不含内部诊断，hint 指向 `diagnose`；空页 → parse 5 |
| `notice_detail_maps_content_links_and_images` | `h1` 标题、发布部门/时间、正文纯文本、附件标题/绝对 URL、正文相对图片按页面 URL 解析、`.gif`/`arrow` 装饰图过滤、`content_truncated=false` |
| `notice_detail_truncates_content_by_characters` | `--max-chars` 按 Unicode 码点截断并置 `content_truncated=true` |
| `notice_detail_falls_back_to_the_page_title_and_normalizes_slash_dates` | 无 `h1` 时回退 `<title>` 的 `-` 前段；`作者：` 与来源/日期边界；`2026/03/09` → `2026-03-09` |
| `notice_detail_ignores_script_bodies_in_content_and_images` | `<script>` 内的伪标签不进入正文与图片列表 |
| `notice_detail_requires_a_title_and_a_content_element` | 缺标题 → parse 5；缺正文容器 → parse 5 |
| `notice_detail_validates_the_destination_before_any_request` | 空/相对/外部域/伪造后缀/`ftp`/非默认端口/URL 内凭据全部 usage 2 且零请求；首尾空白与主机大小写规范化后才发请求 |
| `card_maps_amounts_and_widget_fields` | `qkje/oddfare/mail/a` 映射；GET 卡控件 URL，仅带 `Referer` 与 `X-Requested-With: XMLHttpRequest` |
| `card_keeps_missing_and_malformed_values_null` | 空串/null/非数字 → `null`；数字与数字字符串均可解析；`mail` 小数截断 |
| `card_rejects_non_json_payloads_as_parse_errors` | 非 JSON、数组、字符串 → parse 5，消息不回显字段 |
| `card_login_page_is_an_auth_error` | 200 登录页 → auth 3 |

### 命令编排与会话驱动测试

`src/campus/session.rs` 和 `src/campus.rs` 的测试使用进程内 `ScriptedSession`（实现 `SessionDriver`，底层仍是共享 `Scripted`）与临时合成 `credentials.json`；不读取真实 profile/session，不联网。成功用例断言同一 `run_with_session` 路径、请求顺序、`ensure`/`renew` 次数，以及 credentials 不变、cache 目录为空。这些是命令处理器级证据，不是正式二进制进程级成功输出。

| Rust 测试 | 覆盖 |
|---|---|
| `routes_accept_only_their_exact_shape` | 门户列表类型/URL、卡控件精确 URL 与两个头、详情主机校验；增加 `Cookie` 头即拒绝 |
| `electricity_route_rejects_credentials_and_other_destinations` | `/part` 方法/URL/头/body；换 `/search` 或加 `Authorization` 即拒绝 |
| `scoped_transport_rejects_an_unapproved_request_before_the_driver` | 不匹配 route 的请求在 driver 前 upstream 5，driver/transport 均零调用 |
| `notices_success_runs_through_the_command_without_writing_state` | 默认 `notice,news` 双请求、排序/截断、CLI 数据形状；`ensure=1`、`renew=0`、无状态写入 |
| `notice_detail_success_runs_through_the_command_without_writing_state` | URL 规范化、`--max-chars`、详情字段；无状态写入 |
| `card_success_runs_through_the_command_without_writing_state` | 卡控件请求/字段映射；`ensure(Service::Portal)`；无状态写入 |
| `buildings_success_runs_through_the_command_without_writing_state` | `/part` 表单、area/count/list；`ensure(Service::Electricity)`；无状态写入 |
| `electricity_success_runs_floor_room_and_balance_through_the_command` | 默认 `学一楼/101` 的 `/floor`→`/drom`→`/search` 顺序、partment/floor/meter 表单、CLI 字段；无状态写入 |
| `portal_auth_failure_renews_once_and_restarts_the_list` | 列表首个响应为登录页 → `renew=1`，从第一个类型重新请求后成功 |
| `electricity_auth_failure_renews_only_the_failed_post_once` | `/part` 首次 auth → `renew=1`，只重试 `/part` 后成功 |
| `electricity_stops_after_one_renewal_when_auth_still_fails` | 第二次仍 auth 时返回 auth 3，不无限续期 |
| `default_pending_driver_keeps_the_local_auth_then_upstream_layering` | 默认 `run`：有 profile → upstream 5 且零传输；无 profile → auth 3 |
| `run_rejects_an_unknown_command_before_any_session_use` | 未知命令 usage 2，`ensure`/请求均为零 |
| `command_json_shape_is_not_added_by_the_campus_layer` | 校园层不自行添加 `schema`/`ok`，仍由共享 CLI 包装 |

请求与输出映射（沿用 Python 字段，不扩字段）：

- `notices`：每个类型一次 `GET http://my.bupt.edu.cn/list.jsp?totalpage=30&PAGENUM=0&urltype=tree.TreeTempUrl&wbtreeid=<id>`（`notice` 1154、`news` 1221、`rules` 1536、`ways` 1524）；输出 `{count, types, notices[{title,url,author,time,type,type_name}]}`。`types` 为命令行显式类型，默认时是 `null`。`count=0` 在 `ensure` 后不发起列表请求。
- `notices show`：`GET <校验后的 URL>`，输出 `{notice:{title,url,author,time,content_text,content_truncated,attachments[{title,url}],images[]}}`；不输出 `content_html` 与 `blocks`。
- `card`：`GET http://my.bupt.edu.cn/system/resource/app/cuser/getwxtsA.jsp`，输出 `{student_id,main_wallet,subsidy,unread_mail}`；无法解析的数值保持 `null`（与 Python 一致，见「旧版歧义」）。
- `notices show` 的 URL 只接受 `http`/`https`、主机恰为 `my.bupt.edu.cn`、无显式非默认端口、无 userinfo 的绝对 URL；校验发生在 profile 检查和该模块任何请求之前，实际请求使用规范化后的解析结果。有效 URL 但无凭据时仍返回 auth 3（`notices_show_requires_credentials_without_creating_session`）；空白/外部 URL 即使凭据损坏也先报 usage 2。

**HTML 扫描器边界（未新增依赖）**：扫描器按需覆盖引号属性、注释/声明、void 元素、`script`/`style` 原始文本、同名嵌套深度匹配、文档顺序取最外层匹配，并对文本与属性做实体解码。它不实现 HTML 错误恢复、隐含结束标签、`<p>`/`<li>` 自动闭合或外来内容。若线上页面出现必须依赖浏览器级解析的结构（例如未闭合容器、表格重排、选择器语义变化），再评估引入 `html5ever`/`scraper` 这类维护中的解析器；那会增加依赖，需整合者批准，本轮不预置。

### 匿名图书馆传输（rewrite/transport）

`src/transport.rs` 新增 `Curl` 适配器；`src/cli.rs` 只把顶层 `library` 路由到它，其余命令（`login`、`homework`、`schedule`、`electricity`、`notices`、`card` 等）继续使用 `Offline`。生产路径使用系统 curl（macOS `/usr/bin/curl`，Windows 优先 `%SystemRoot%\System32\curl.exe`），不新增依赖或运行时包。

安全与边界：

- 只接受 `POST https://opac.bupt.edu.cn/search-full-result.json`、`POST https://opac.bupt.edu.cn/dianzi.json` 和 `GET https://opac.bupt.edu.cn/bookInfo_<id>.html`。`<id>` 只允许 ASCII unreserved 与 UTF-8 百分号字节；`%2F`、`%20` 等编码后的 ASCII 一律拒绝，避免编码路径分隔符或二次解码逃出 route 模板。
- 请求头只允许 `User-Agent`、`Referer`、`X-Requested-With`、`Content-Type`；重复头、Cookie/Authorization/未知头和含 CR/LF 的值一律拒绝；`Referer` 只接受 OPAC 搜索页或馆藏页。
- 首个参数固定 `--disable`，不读 `.curlrc`；`--proxy ""` 忽略代理环境；`--proto =https`；不传 `--location` 且显式 `--max-redirs 0`；不传 `--retry`。
- 请求体经 curl stdin 传入，不进入 argv；参数由 `std::process::Command` 直接传递，不经过 shell。
- `--max-time` 最多 120s、`--connect-timeout` 最多 10s、`--max-filesize` 8 MiB；Rust 侧管道读取再限制为 8 MiB 加状态标记，超限即终止子进程。
- curl stdout 末尾用 `__BUPT_HTTP_STATUS__` ASCII 分隔符加三位 HTTP 状态；curl 退出非 0 一律返回 transport failure（network 4），退出 0 时保留真实 HTTP 状态（含 4xx/5xx）。错误消息不含请求、响应或 curl stderr 内容。

本轮传输测试（全部离线，无真实校园网）：

| Rust 测试 | 覆盖 |
|---|---|
| `builds_a_bounded_argument_vector_for_library_search` | 参数顺序、`--disable` 首位、超时/大小/无重定向参数、四个允许头、无 `--location`/`--retry`/`--cookie`，请求体不进入 argv |
| `caps_the_request_timeout_and_connect_timeout` | 120s/10s 上限、亚秒值、零超时拒绝 |
| `accepts_only_documented_methods_for_exact_library_routes` | 三条 route 的 GET/POST 组合；错误方法、GET 带 body、超长 body 拒绝 |
| `rejects_urls_outside_the_exact_library_routes` | http、伪装/带端口/带 userinfo 主机、query/fragment、任意路径、空 id、路径分隔符、坏百分号、空格；UTF-8 id 仍接受 |
| `rejects_credentials_unknown_headers_and_header_injection` | Cookie/Authorization/Proxy-Authorization/未知/重复头、坏 Referer、坏 X-Requested-With、坏 Content-Type、CRLF 注入；小写头名可用 |
| `parses_the_status_from_the_final_write_out_marker` | 状态和 body 分离、body 含分隔符/换行/数字、malformed 状态、超限 body |
| `reads_at_most_the_bounded_number_of_response_bytes` | 管道读取上限包含等于上限、拒绝多一字节 |
| `send_preserves_the_http_status_and_calls_curl_once` | HTTP 503 仍返回 `Response`，只调用一次 curl |
| `send_does_not_retry_a_transport_failure` | transport failure 只尝试一次，不消费第二次响应 |
| `nonzero_curl_exit_is_a_transport_failure_even_with_a_status_line` | 退出非 0 即使已有状态行也归 network 4，且不回显 body |
| `invalid_requests_do_not_reach_the_curl_process` | route 拒绝发生在 spawn 前，upstream 5 |
| `malformed_or_oversized_output_is_a_transport_failure` | 缺状态标记、超限 synthetic 输出均为 network 4 |
| `system_curl_receives_separate_arguments_and_the_body_on_stdin`（Unix） | 用临时 fake curl 进程验证真实 spawn：URL/含 shell 元字符的头值各为一个 argv，body 只走 stdin，状态标记被剥离 |
| `library_search_uses_the_injected_public_transport`（`src/cli.rs`） | 正式 CLI 路由把 `library search` 交给注入的 public transport，`Offline` 零调用，输出仍是 `schema: "bupt.v1"` |
| `only_the_library_lane_uses_the_public_network`（`src/cli.rs`） | 当前公开命令中只有顶层 `library` 选择 public transport |

已知限制：

- **Windows 实际执行未验证**：`%SystemRoot%\System32\curl.exe` 路径、Windows argv 引号/控制字符、进程终止和 stdin 管道都需要在 Windows 实机复跑；Unix fake curl 测试不覆盖 Windows。
- **没有 Rust 侧真实 OPAC 实测**：Python 基线曾返回 HTTP 200 和 3 条公开结果，但本轮按任务要求不访问真实校园（含公共图书馆），因此不能声称 Rust live pass。
- **命令层错误分层仍未接通**：`src/campus/library.rs` 的 `library_send` 仍把所有 transport error 映射为 usage 2；适配器内部已区分 HTTP 状态与 transport failure，但 CLI 退出码仍是旧行为。
- **响应头未暴露**：`Curl` 返回空 `headers`，现有图书馆解析器不读取响应头；若后续需要 `Content-Type`/重定向诊断，要扩展适配器。
- **代理被禁用**：`--proxy ""` 保证不把环境代理凭据带进请求，但也意味着需要代理才能出网的 Windows 环境会失败。
- **异常 id 失败关闭**：含编码 ASCII（如空格、`/`、`?`、`%`）的馆藏 id 会在 transport 前被拒绝，普通 OPAC 字母数字 id 不受影响。
- **3xx 不跟随**：上游若返回重定向，会作为 HTTP 状态交给 `ensure_success`，最终按旧行为成为 usage 错误，而不是自动跳转。
- **依赖系统 curl**：macOS/Windows 缺少 curl 时返回 network 4，提示检查网络；没有内置下载或打包 curl。

验证记录见文末「验证记录」；本轮没有运行真实校园网、浏览器、登录或公共图书馆请求，也没有新增环境变量回放或公开 CLI 参数。

## 仍需父会话层接通的场景

以下不是校园模块内部缺失，而是父持有的 `src/cli.rs` / `src/transport.rs` / `src/identity` 接口尚未提供真实 `SessionDriver`。没有写成 `#[ignore]` 或永远通过的假测试：

1. **父会话驱动实现**：实现 `campus::SessionDriver`：
   - `ensure(&mut self, service: Service)`：按 `Service::Portal` / `Service::Electricity` 读取/验证本地会话，完成对应的门户或电费会话 prime/CAS 握手；校园模块只从 `credentials.json` 检查 username + 非空密码，不要求 role。
   - `send(&mut self, route: Route, request: Request)`：先复核 `route.accepts(&request)`，再附加 Cookie/token 并执行一次 HTTP 交换；HTTP 状态（含 401/403/3xx）要作为 `Response` 返回给校园层分类，只有传输失败才返回 network 4。不得在 `Request` 里加凭据，不得隐式重试或跟随重定向。
   - `renew(&mut self, service: Service)`：只在校园层已确认 auth 3 后，按该 service 失效并刷新一次会话；不得把凭据发到未批准目的地。
2. **CLI 接入点**：父层调用 `campus::run_with_session`（或把 `campus::run` 的 `PendingSession` 换成真实驱动）。当前 `cli.rs` 的 `uses_campus_transport` 仍未把 `electricity` / `notices` / `card` 路由到带 Cookie 的传输，所以正式二进制仍是 `Offline` 语义；本批未改父文件。
3. **scheme/WebVPN 决策**：`Route` 当前按 Python 固定门户 `http://my.bupt.edu.cn` 和电费 `https://app.bupt.edu.cn` 的精确形状校验。若父层要改用 HTTPS/WebVPN 或代理，必须先修改并评审 route 模板，不能由驱动静默改写 URL；本轮不猜测。
4. **cookie/redirect 实际边界**：真实 401/403 后的会话失效、3xx 是否跟随、Cookie 域/路径、WebVPN 地址仍未实测；校园层只按确认的 `e/m` 标记或 401/403 进入 auth 3，3xx 不猜成登录页。若未来跟随重定向，必须在每一跳重新校验 `Route` 目标并重新确认 Cookie 域。
5. **进程级成功输出**：共享 CLI 负责 `schema: "bupt.v1"` 和 `--pretty`；本批内部命令测试验证的是 `Outcome` 数据对象，尚未通过正式二进制回放门户/电费成功响应。进程合同测试仍只覆盖本地参数、无 profile 的 auth 层和文件副作用。
6. **library 上游错误分层**：传输错误、HTTP 非 2xx、`result.code != 0`、hold 404/500、JSON 形状错误。本轮仍按 Python 旧版全部归为 usage 2；是否改为 network/upstream 分层仍未决定。

## 会话驱动的安全约束

- `notices show` 已在 `src/campus/portal.rs` 内校验目标：只接受 `http`/`https`、主机恰为 `my.bupt.edu.cn`、无显式非默认端口、无 userinfo 的绝对 URL；校验先于 profile 检查和该模块的任何请求，请求使用规范化后的解析结果。
- `Route::accepts` 在任何 `SessionDriver::send` 前固定方法、完整 URL/主机、请求头集合和空/非空 body；`ScopedTransport` 在驱动前再次拒绝不匹配请求。门户列表/卡/详情和电费四个 endpoint 都有对应 route。
- 校园模块不把 Cookie、Authorization 或 token 放进 `Request`；卡控件只设置 Python 要求的 `Referer` 与 `X-Requested-With`，电费只设置 `Referer`、`X-Requested-With`、`Content-Type`。驱动应在自己的传输层附加会话材料，而不是修改请求头。
- 重定向仍不跟随；驱动若要跟随，必须在每一跳复核目标仍匹配原 `Route`，且不得把 Cookie 带到其他主机/端口/userinfo。
- 不得把校园凭据、Cookie 或 token 发送到任意 `--url` 指向的地址。
- 已知 Python 门户入口使用 `http://my.bupt.edu.cn`（校内主机），电费使用 `https://app.bupt.edu.cn`。若父层决定改用 HTTPS/WebVPN，必须先评审并更新 route；本轮不发送任何真实 cookie 或密码，也不猜测该决定。
- electricity 不跟随或解释重定向；3xx 交由响应体解析，登录页不会被猜成 auth，只有确认的 `e/m` 标记或 401/403 才进入 auth。
- 确认 auth 后只允许一次 `renew`；门户列表从第一个类型重启，电费只重试失败的单次 `/post`。第二次 auth 直接返回 3。

## 解析与编码边界

这些测试证明的是合成夹具上的字段映射和请求形状，不是与线上站点逐字一致：

- HTML 只做简单的标签/实体扫描，不覆盖畸形或深度嵌套 HTML、复杂表格合并单元格等；线上结构变化可能改变 `page_title`、馆藏行或正文提取。
- 路径段编码按 UTF-8 字节百分号编码，空格在路径用 `%20`、表单用 `+`；未验证上游是否接受所有保留字符 ID，也未做 URL 规范化。
- 搜索 `data` 非数组时当前按空列表处理；浮点计数截断；这些形状变化没有线上回归证据。
- 电费数值按 Python 的 `or` 默认规则处理已测试的空/零标量；非法字符串和 NaN/Infinity 会转为默认值或 null，但没有单独夹具，也没有线上对照。
- 电费楼层推断按 ASCII 数字分组实现 Python `re.findall(r"\d+")` 的常见输入；Python 的 Unicode `\d`/`isdigit()` 边界（非 ASCII 数字）没有夹具，可能与 Rust 的 ASCII 判断不同。
- 电费房间规范化实现 Python 的 `[\s_-]+` 去除和 `.upper()`，但 Python 的 Unicode 大小写/空白全集没有逐码点对照；合成测试只覆盖 ASCII 与少量中文。
- 查询链按单次 CLI 调用实现，没有移植 Python 的进程内楼层/房间元数据缓存；重复查询会重复 `/floor`、`/drom`，结果预计一致但没有线上对照。
- 电费 `/floor`、`/drom` 列表项现在逐项要求 JSON object；Python 的 `/floor` 非对象项会抛未包装异常，Rust 统一为 parse 5。`/part` 列表仍原样透传，不逐项验证对象形状。
- 门户 HTML 用自写扫描器（无新依赖），只覆盖合成夹具与合同列出的结构；不实现 HTML 错误恢复、隐含结束标签、表格重排等浏览器级行为。列表缺 `newslist`、条目全无效、详情缺标题/正文容器都按 parse 5 处理；href 无法解析的条目会被跳过，若全部跳过则报「no valid notice items」。
- 门户卡片数字按 Python 的静默容错：无法解析或非有限值 → `null`；`mail` 小数向零截断。Python 对 `NaN`/`Infinity` 会输出非法 JSON，Rust 侧固定输出合法 JSON 的 `null`，无线上对照。
- 正文 `content_text` 的空白按 Python `html_to_text` 保留内部空白（标签替换为空格后仅去首尾）；`<script>`/`<style>` 的原始文本被排除，这一点比 Python 的正则剥离更严格。
- 门户响应按 UTF-8 有损解码（与图书馆路径一致）；若上游实际用 GBK/GB2312，需要 charset 解码依赖，属于新增依赖决策，本轮不猜。

## 旧版歧义（未在本轮决定）

1. **library 上游错误退出码**：Python `cmd_library_search` / `cmd_library_hold` / `cmd_library_ebook` 把 `LibraryError` 统一转成 `UsageError`（退出 2），包括传输错误、上游返回 `result.code != 0` 和 hold 页面 404/500。当前实现为保持旧行为暂时沿用 usage 2；内部测试只固定该分类和通用消息，进程合同未覆盖错误分层。Rust 合同的分层错误可能更适合 network/upstream，仍待整合者决定。
2. **`notices --count 0`**：Python 仍先建立会话（无凭据时 auth 3），然后客户端短路返回空列表。核心实现了「有凭据后不发起请求直接返回空列表」，生产命令接适配器时仍需先过凭据层；是否允许完全无会话的空查询仍未决定。
3. **损坏 config 与完整 flag 的关系**：Python `resolve_dorm` 无条件读取 `config.json`，因此即使 flag 已给全，损坏配置仍会让查询失败。本轮验证了「参数本身非法时先报参数错误」「buildings 不因 dorm config 失败而抢先」，以及「无 flag/env 时 config 中的无效楼名会触发 usage（证明配置被读取）」；没有决定完整 flag 是否应绕过损坏配置；默认参数下的完整电表请求来源已由命令级测试固定，损坏配置与完整 flag 的组合仍未决定。
4. **`electricity buildings` 是否需要登录**：Python 走 `_session(required=True)`，所以无凭据是 auth 3。若上游元数据实际可匿名读取，这是合同未写明的新行为；本轮按旧版断言 auth。
5. **card 数字容错**：Python 把无法解析的 `qkje/oddfare/mail` 静默变成 null。核心按 Python 保留静默容错并已由测试固定；是否改为 parse 错误仍未决定，改行为需整合者确认。
6. **notices 详情输出字段**：CLI 只输出 `title/url/author/time/content_text/content_truncated/attachments/images`，不输出 `content_html` 与 `blocks`。本轮按「保留能沿用的字段」记录，不扩字段；`src/campus/portal.rs` 的输出与此一致。

## 验证记录

执行目录：`/Users/limoon/Documents/moon_cli_or_tool/.worktrees/bupt-rust/campus`

```sh
cargo fmt --check
cargo clippy --locked --offline --all-targets
cargo test --locked --offline --lib
cargo test --locked --offline --test contract_campus
cargo test --locked --offline
```

- `cargo fmt --check`：通过。
- `cargo clippy --locked --offline --all-targets`：通过编译；`src/campus.rs`、`src/campus/session.rs`、`src/campus/electricity.rs` 与 `src/campus/portal.rs` 无 clippy 告警，其余告警是本线外既有项。
- `cargo test --locked --offline --lib`：210 passed（此前 192；新增 18 个 `campus` 测试：11 个命令编排/驱动 + 6 个电费查询链 + 1 个楼栋元数据）。
- `cargo test --locked --offline --test contract_campus`：34 passed，进程用例保持原分类（无凭据 auth 3、未知类型/URL usage 2、有 profile 停在 pending upstream 5）。
- `cargo test --locked --offline`：341 passed，0 failed；lib 210 + academics 30 + campus 34 + homework 16 + identity 28 + lifecycle 23。
- 未新增 Cargo 依赖、公开 CLI 参数、环境变量回放或会话文件；未读取真实 profile/session，未访问校园网或 GitHub，未提交真实 cookie/密码。
- 上述未接通场景没有对应的红测，也没有用 `#[ignore]`/`#[should_panic]` 隐藏。
