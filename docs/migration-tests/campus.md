# campus lane：测试迁移清单

范围：`library search|hold|ebook`、`electricity`、`electricity buildings`、`notices`、`notices show`、`card`。
Python 基准：`d7c35ef` 的 `tests/test_library.py`、`tests/test_electricity.py`、`src/bupt_cli/{library,commands,config}.py`，以及 `src/bupt_cli/_vendor/campus_api/bupt/{portal,electricity,models,constants}.py`。
Rust 合同：`docs/rust-migration.md`。本文件记录测试、实现边界与未决项；合同冲突时以 `docs/rust-migration.md` 为准。

## 已执行的离线用例

`tests/contract_campus.rs` 全部通过公开 CLI 进程边界验证，使用共享 `Sandbox`，只用合成输入，不读取真实凭据、不依赖校园网响应。所有错误用例都断言：stdout 为空、stderr 是单个 JSON、带 `schema: "bupt.v1"`、`ok: false`、正确的 `error.type` 和退出码。进程级用例没有成功响应注入，只能区分本地参数、配置和凭据层，不能证明实现没有发起请求；库解析和请求形状由 `src/campus/library.rs` 内的 `Scripted` 测试覆盖。

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

`tests/fixtures/contracts/campus/` 是脱敏的上游响应形状；`src/campus/library.rs` 的内部 `Scripted` 传输测试已直接消费图书馆响应。电费/门户夹具继续留给会话适配器接通后的测试。

| 夹具 | 预期用途 |
|---|---|
| `library_search.json` / `library_search_empty.json` | 书目结果字段映射、高亮标签/实体清理、坏计数为 null、空结果 |
| `library_holdings.html` | 馆藏行：固定状态列、列偏移回退、非馆藏行过滤、可借计数 |
| `library_ebook.json` / `library_ebook_empty.json` | 扁平 `{locate, recNote}`、`\x1e` 清理、空列表 `has_electronic=false` |
| `portal_notices_notice.html` | 通知列表：相对 URL 补全、author/time、空 href 过滤 |
| `portal_notice_detail.html` | 详情：标题、发布部门/时间、正文、相对图片、装饰图过滤、附件 |
| `portal_card.json` / `portal_card_invalid.txt` | 卡余额字段映射；非 JSON 错误 |
| `portal_login_page.html` | 门户登录页识别（auth 分类，而不是 parse/network） |
| `electricity_buildings_shahe.json`、`electricity_buildings_empty.json` | 楼栋列表字段原样映射、空列表 `count=0` |
| `electricity_floors.json`、`electricity_rooms.json` | 后续楼层/房间解析的合成形状；本轮未消费 |
| `electricity_meter.json`、`electricity_meter_empty.json` | 电表余额字段映射；空/零值的默认值、`freeEnd` 回退 |
| `electricity_auth_error.json` | 上游 `e/m` 未登录标记 |
| `electricity_upstream_error.json` | `e != 0` 且无登录标记的维护错误 |

## 第一批实现（本地路径）

`src/campus.rs` 保留命令注册、路由、参数校验和占位 portal 桥；`src/campus/library.rs` 负责匿名 OPAC；`src/campus/dorm.rs` 负责宿舍配置与楼名；`src/campus/electricity.rs` 负责电费响应到 CLI 字段的离线映射。已实现：

- `library search|hold|ebook` 的参数规格：`--page`、`--page-size`、`--location`、`--with-ebook`、位置参数与空白校验。
- 匿名 OPAC 请求和响应解析：书目字段、HTML 高亮/实体清理、馆藏状态列与偏移回退、电子资源扁平列表和 `\x1e` 清理；`--with-ebook` 的旁路查询失败只记 `null`。
- `electricity` / `electricity buildings` 的参数规格、楼名与校区别名、`config.json` 的 `dorm` 解析，以及 flag > env > config > built-in placeholder 的优先级。
- `electricity buildings` 的 `area`/`count`/原样列表映射，以及余额的 `building/floor/room/surplus/price/remaining_kwh/total_consumed_kwh/meter_time/low_balance/threshold/source` 映射。`src/campus/electricity.rs` 提供 typed `MeterTarget` 和基于共享 `Transport` 的 Scripted 测试缝；生产会话/楼层房间解析尚未接线，`run_electricity` 有 profile 时仍停在 pending。
- `notices` / `notices show` / `card` 的本地参数校验与类型筛选；`notices show` 的空白 URL 在读取 profile 前以 usage 2 拒绝。受保护命令统一使用 `identity::require_authenticated_profile`，只从临时目录的 `credentials.json` 读取 username 和非空密码，不读取账号环境变量，也不要求 role；无本地 profile 或凭据损坏时返回 auth 3，有 profile 时停在 session adapter pending，不伪造成功。匿名 `library` 路径不读取凭据。

### 本轮新增的内部 fixture 测试

这些测试在 `src/campus/library.rs` / `src/campus/dorm.rs` / `src/campus/electricity.rs` 的 `#[cfg(test)]` 中；图书馆和电费映射测试通过共享 `Scripted` 传输回放合成夹具，不联网：

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
| `buildings_maps_area_count_and_response_items` | `/part` 的 `areaid` 请求、shahe/xitucheng 回显、空列表、楼栋字段原样保留 |
| `balance_maps_fixture_fields_and_response_overrides` | `/search` 表单、响应 `parName/floorName` 覆盖 fallback、余额/价格/剩余量/总用量/时间/阈值/source 映射 |
| `balance_keeps_empty_and_zero_values_compatible` | 空 surplus、零/空 price、空 `vTotal` 回退 `freeEnd`、空名称/时间、低余额阈值 |
| `confirmed_login_markers_are_auth_errors` | 四个确认登录标记在 `e != 0` 时映射 auth 3，不把普通错误猜成 auth |
| `nonzero_status_without_auth_marker_keeps_python_classification` | 维护错误为 network 4；`请登录` 近似文本不升级为 auth；`无数据` 为 not_found 5 |
| `malformed_payloads_are_parse_errors` | 非对象、缺 `e`、缺 `d.data`、错误数据类型均为 parse 5 |
| `invalid_json_is_a_parse_error` | 非 JSON 响应为 parse 5 |

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

本轮验证：

```sh
cargo fmt --check
cargo test --locked --offline
```

- `cargo fmt --check`：通过。
- `cargo test --locked --offline`：通过；89 个库测试 + 25 academics + 33 campus + 14 homework + 28 identity + 23 lifecycle，共 212 个 Rust 测试，0 失败。
- 没有运行真实校园网、浏览器、登录或公共图书馆请求；没有新增环境变量回放或公开 CLI 参数。

## 仍未接通的场景

这些场景没有写成 `#[ignore]` 或永远通过的假测试；当前没有对应的红测在失败，等会话/传输能力接通后再补：

1. **library 上游错误分层**：传输错误、HTTP 非 2xx、`result.code != 0`、hold 404/500、JSON 形状错误。本轮按 Python 旧版全部归为 usage 2；是否改为 network/upstream 分层仍未决定。
2. **electricity 余额完整请求链**：楼层推断/楼层 ID 查询、房间匹配与 `dromNum`、楼栋 `partmentId`/`areaId` 元数据，以及会话适配器。响应到 CLI 的字段映射已由 typed `MeterTarget` 离线覆盖；`run_electricity` 尚未接线。
3. **electricity buildings 生产接线**：`areaid` 请求和列表映射已离线覆盖，仍缺会话适配器、真实传输和进程级成功输出。
4. **electricity 会话/HTTP 边界**：401/403 的会话失效与是否续期、3xx/登录页处理、真实 cookie；本轮只按合成 `e/m` 和 HTTP 状态映射，不猜测登录重定向。
5. **notices 列表成功**：默认 `notice,news`、显式类型、按时间倒序、`--count` 截断、列表缺失/空列表 parse 错误、明确过期后最多续期一次。
6. **notices 详情成功**：`title/url/author/time/content_text/attachments/images`、`--max-chars` 截断、正文/标题缺失错误、相对 URL 解析。
7. **card 成功/错误**：`qkje/oddfare/mail/a` 映射、无效数字为 null、非 JSON/非对象/登录页、明确过期后最多续期一次。
8. **进程级成功输出**：共享 CLI 负责 `schema: "bupt.v1"` 和 `--pretty`；本轮内部测试验证的是数据对象，尚未通过正式二进制回放成功响应。

进程测试当前只能验证参数、无 profile 的 auth 层和文件副作用；不能证明实现没有发请求。成功响应测试需要整合者提供的进程内传输注入或等价 seam，生产二进制不增加模拟环境变量。

## 待门户适配器落实的安全约束

- `notices show` 目前只拒绝空白 URL；接 portal adapter 时必须校验目标在允许的校园门户域名/路径内，重定向后也要复核。
- 不得把校园凭据、Cookie 或 token 发送到任意 `--url` 指向的地址。
- 本轮只做本地空白 URL 校验，未连接 portal adapter、未发起真实请求，也未实现域名白名单。
- electricity 映射不跟随或解释重定向；3xx 交由响应体解析，登录页不会被猜成 auth，只有确认的 `e/m` 标记或 401/403 才进入 auth。

## 解析与编码边界

这些测试证明的是合成夹具上的字段映射和请求形状，不是与线上站点逐字一致：

- HTML 只做简单的标签/实体扫描，不覆盖畸形或深度嵌套 HTML、复杂表格合并单元格等；线上结构变化可能改变 `page_title`、馆藏行或正文提取。
- 路径段编码按 UTF-8 字节百分号编码，空格在路径用 `%20`、表单用 `+`；未验证上游是否接受所有保留字符 ID，也未做 URL 规范化。
- 搜索 `data` 非数组时当前按空列表处理；浮点计数截断；这些形状变化没有线上回归证据。
- 电费数值按 Python 的 `or` 默认规则处理已测试的空/零标量；非法字符串和 NaN/Infinity 会转为默认值或 null，但没有单独夹具，也没有线上对照。
- 电费 `MeterTarget` 依赖调用方已经解析出的楼层/房间 ID 与显示 fallback；本轮不实现楼层推断、房间列表匹配或楼栋元数据。
- 电费列表项原样透传，不逐项验证对象形状；notices 列表/详情与 card 仍未接入响应解析。

## 旧版歧义（未在本轮决定）

1. **library 上游错误退出码**：Python `cmd_library_search` / `cmd_library_hold` / `cmd_library_ebook` 把 `LibraryError` 统一转成 `UsageError`（退出 2），包括传输错误、上游返回 `result.code != 0` 和 hold 页面 404/500。第一批实现为保持旧行为暂时沿用 usage 2；内部测试只固定该分类和通用消息，进程合同未覆盖错误分层。Rust 合同的分层错误可能更适合 network/upstream，仍待整合者决定。
2. **`notices --count 0`**：Python 仍先建立会话（无凭据时 auth 3），然后客户端短路返回空列表。是否应允许无会话的空查询未定，本轮未测。
3. **损坏 config 与完整 flag 的关系**：Python `resolve_dorm` 无条件读取 `config.json`，因此即使 flag 已给全，损坏配置仍会让查询失败。本轮验证了「参数本身非法时先报参数错误」「buildings 不因 dorm config 失败而抢先」，以及「无 flag/env 时 config 中的无效楼名会触发 usage（证明配置被读取）」；没有决定完整 flag 是否应绕过损坏配置，也没有验证最终电表请求的来源。
4. **`electricity buildings` 是否需要登录**：Python 走 `_session(required=True)`，所以无凭据是 auth 3。若上游元数据实际可匿名读取，这是合同未写明的新行为；本轮按旧版断言 auth。
5. **card 数字容错**：Python 把无法解析的 `qkje/oddfare/mail` 静默变成 null。是否保留静默容错还是报 parse 错误，本轮未决定。
6. **notices 详情输出字段**：CLI 只输出 `title/url/author/time/content_text/content_truncated/attachments/images`，不输出 `content_html` 与 `blocks`。本轮按「保留能沿用的字段」记录，不扩字段。

## 验证记录

执行目录：`/Users/limoon/Documents/moon_cli_or_tool/.worktrees/bupt-rust/campus`

```sh
cargo fmt --check
cargo test --locked --offline --lib
cargo test --locked --offline --test contract_campus
cargo test --locked --offline
```

- `cargo fmt --check`：通过。
- `cargo test --locked --offline --lib`：81 passed（含 campus 的 library/dorm/electricity 离线测试）。
- `cargo test --locked --offline --test contract_campus`：33 passed。
- `cargo test --locked --offline`：204 passed，0 failed；覆盖 lib 与全部离线 contract 测试。
- 上述未接通场景没有对应的红测，也没有用 `#[ignore]`/`#[should_panic]` 隐藏；本轮没有真实校园网请求。
