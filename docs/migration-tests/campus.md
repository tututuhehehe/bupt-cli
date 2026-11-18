# campus lane：测试迁移清单

范围：`library search|hold|ebook`、`electricity`、`electricity buildings`、`notices`、`notices show`、`card`。
Python 基准：`d7c35ef` 的 `tests/test_library.py`、`tests/test_electricity.py`、`src/bupt_cli/{library,commands,config}.py`，以及 `src/bupt_cli/_vendor/campus_api/bupt/portal.py`。
Rust 合同：`docs/rust-migration.md`。本文件只记录测试阶段，不含实现决定。

## 已执行的离线用例

`tests/contract_campus.rs` 全部通过公开 CLI 进程边界验证，使用共享 `Sandbox`，不读取真实凭据、不访问校园网。所有错误用例都断言：stdout 为空、stderr 是单个 JSON、带 `schema: "bupt.v1"`、`ok: false`、正确的 `error.type` 和退出码。

| Rust 用例 | 覆盖语义 | Python 来源 |
|---|---|---|
| `library_search_rejects_blank_keyword_without_reading_credentials` | 空白/空关键词在任何凭据读取前以 usage 失败；错误点名「关键词」 | `test_search_rejects_blank_keyword`，`library.py:search` |
| `library_hold_and_ebook_reject_blank_ids_without_reading_credentials` | hold/ebook 空 id 在凭据读取前以 usage 失败；错误点名 `recCtrlId` | `test_holdings_rejects_blank_id`、`test_electronic_resources_rejects_blank_id` |
| `library_requires_explicit_subcommands_and_arguments` | `library`、`search`、`hold`、`ebook` 缺子命令/位置参数均为 usage | `cli.py` library 分派 |
| `library_rejects_invalid_pagination_before_any_state_or_network` | `--page` / `--page-size` 下界、非整数、`--timeout 0` 在状态读写前拒绝 | `cli.py:_int_at_least`、`_positive_float` |
| `electricity_default_placeholder_needs_a_campus_session` | 无参数使用占位宿舍后进入凭据层，返回 auth 3；不创建会话缓存 | `test_default_dorm_is_xitucheng_xue1_101` 的本地部分 |
| `electricity_building_flag_requires_a_room_without_falling_back` | 传 `--building` 时即使 env/config 有 room 也要求 `--room` | `test_building_without_room_is_usage_error`、`resolve_dorm` |
| `electricity_rejects_unknown_building` | 未知楼名是 usage，提示指向 `electricity buildings` | `test_unknown_building_is_usage_error` |
| `electricity_accepts_documented_building_aliases_before_auth` | 学五楼/学5楼/学5/5/5楼/学十一楼/学29楼/SHAHE_A/shahe_a/沙河校区雁北园A楼均可本地解析并进入凭据层 | `test_building_aliases` |
| `electricity_environment_building_wins_over_config` | env building 覆盖 config，且不会因 config 楼名无效而失败 | `test_env_beats_config` |
| `electricity_flag_building_wins_over_environment` | flag building 覆盖 env | `test_flags_beat_env_and_config` 的楼名分支 |
| `electricity_config_building_is_used_when_flags_and_env_are_absent` | 无 flag/env 时读取 config 中的楼名 | `test_config_used_when_no_flag_or_env` |
| `electricity_rejects_legacy_area_on_the_query_command` | 顶层 `electricity --area` 是 usage，提示改用 `electricity buildings`；不读配置 | `test_area_is_rejected_on_top_level_electricity` |
| `electricity_buildings_rejects_unknown_area_before_any_state` | `buildings --area mars` 在凭据/配置前拒绝 | `test_buildings_subcommand_rejects_bad_area` |
| `electricity_buildings_ignores_malformed_dorm_config` | 损坏的 `config.json` 不阻塞 `buildings`；无凭据时返回 auth 而不是 config 错误 | `test_buildings_subcommand_ignores_broken_config` |
| `electricity_buildings_accepts_documented_area_aliases` | shahe/沙河/2/xitucheng/haidian/西土城/海淀/1 均本地解析；不读 dorm config | `_resolve_area`、`test_buildings_subcommand_accepts_xitucheng` |
| `electricity_rejects_invalid_thresholds` | 负值、NaN、inf、非数字为 usage | `cli.py:_float_at_least` |
| `electricity_accepts_zero_threshold_before_auth` | 阈值 0 合法，继续到凭据层 | `test_default_dorm_query_shape` 的 threshold 边界 |
| `electricity_reports_malformed_config_as_usage_error` | 损坏 JSON 是 usage，消息点名 config，提示提到 JSON | `test_bad_config_json_is_usage_error` |
| `electricity_rejects_non_object_config_and_dorm_sections` | 顶层非对象、`dorm` 非对象均为 usage | `test_non_object_dorm_config_is_usage_error` |
| `electricity_reports_unreadable_config_without_json_hint` | 路径是目录时报告不可读/权限，而不是 JSON 语法错误 | `test_unreadable_config_is_usage_error_not_json_hint` |
| `notices_requires_credentials_before_network` | 无凭据时 notices 返回 auth 3，提示登录；不创建会话 | `commands.py:cmd_notices_list` |
| `card_requires_credentials_before_network` | 无凭据时 card 返回 auth 3，提示登录；不创建会话 | `commands.py:cmd_card` |
| `notices_rejects_unknown_types_before_auth` | 未知/混合类型在凭据前 usage，提示列出类别；尾随逗号按旧版忽略，不在此列 | `commands.py:_notice_types` |
| `notices_accepts_case_insensitive_types_before_auth` | 类型大小写不敏感，合法类型继续到凭据层 | `_notice_types`、`NoticeType` |
| `notices_rejects_invalid_count_as_usage_error` | `--count` 负数/非整数/小数在凭据前 usage | `test_invalid_numeric_arguments_are_usage_errors` |
| `notices_show_requires_a_url_and_non_negative_max_chars` | 缺 URL、负 `--max-chars` 为 usage | `cli.py` notices show 参数 |
| `notices_show_requires_credentials_before_fetching_detail` | 有效 URL 但无凭据时 auth 3；使用 `.invalid` 合成 URL，不发请求 | `commands.py:cmd_notices_show` |
| `notices_and_card_do_not_read_the_dorm_config` | 损坏 dorm config 不影响 notices/card 的凭据错误 | `commands.py` portal 分派 |
| `card_rejects_unexpected_arguments` | card 不收 `--area`/位置参数 | `cli.py` card 参数 |

### 边界与副作用断言

- 所有 usage/auth 用例都断言没有 `cache/session.json` 新文件。
- library 的空白输入用例故意放一个损坏的 `credentials.json`；library 仍报 usage，证明匿名校验不读账号配置。
- `electricity buildings` 用例故意放损坏的 `config.json`；报 auth 而不是 config，证明楼栋列表路径不依赖宿舍默认值。
- 缺 room、顶层 `--area` 用例故意放损坏配置；报具体参数错误，证明这些参数校验先于业务配置读取。未知楼名在 Python 中位于配置读取之后，未用损坏配置做顺序断言。
- 所有断言只使用合成输入：楼名是公开常量或占位符，URL 使用 `example.invalid`，没有真实学号、密码、Cookie、token 或校园网请求。

## 合成夹具

`tests/fixtures/contracts/campus/` 是脱敏的上游响应形状，供后续接入离线传输缝后使用；当前没有测试直接消费它们，避免用假测试替代未定的注入机制。

| 夹具 | 预期用途 |
|---|---|
| `library_search.json` / `library_search_empty.json` | 书目结果字段映射、高亮标签/实体清理、坏计数为 null、空结果 |
| `library_holdings.html` | 馆藏行：固定状态列、列偏移回退、非馆藏行过滤、可借计数 |
| `library_ebook.json` / `library_ebook_empty.json` | 扁平 `{locate, recNote}`、`\x1e` 清理、空列表 `has_electronic=false` |
| `portal_notices_notice.html` | 通知列表：相对 URL 补全、author/time、空 href 过滤 |
| `portal_notice_detail.html` | 详情：标题、发布部门/时间、正文、相对图片、装饰图过滤、附件 |
| `portal_card.json` / `portal_card_invalid.txt` | 卡余额字段映射；非 JSON 错误 |
| `portal_login_page.html` | 门户登录页识别（auth 分类，而不是 parse/network） |
| `electricity_buildings_shahe.json`、`electricity_floors.json`、`electricity_rooms.json`、`electricity_meter.json` | 楼栋/楼层/房间/电表查询链与输出字段 |
| `electricity_auth_error.json` | 上游 `e/m` 未登录错误分类 |

## 待实现的集成场景（需要整合者先定离线缝）

Rust 集成测试目前不能在不联网的前提下替换 HTTP 响应。需要的**最小测试缝**：
按 method + URL/path 返回预置响应（含状态码、headers、body），能捕获出站表单/查询参数，且只在测试进程中启用；不要求生产环境变量或新的公开命令。缝的名称与位置由整合者决定，本 lane 不擅自新增公开行为。

缝就绪后应补的用例：

1. **library search 成功**：夹具字段映射为 `id/title/authors/isbn/publisher/year/call_number/library/holding_count/available_count`；坏计数为 `null`；请求含 `solrType=TITLE`、`xiaoqu=all`、`keyword`、`pageNo`、`pageSize`；空结果为 `total=0`、`books=[]`。
2. **library hold 成功**：状态列优先、列偏移回退、无状态词为空串、非校区行过滤、`count`/`available`/`page_title`。
3. **library ebook 成功/空**：两条资源、`\x1e` 去除、顺序稳定；空列表为 `count=0`、`has_electronic=false`。
4. **library 错误分层**：`result.code != 0`、hold 404/500、传输超时；同时解决下方「旧版歧义」。
5. **library `--with-ebook`**：单条电子资源查询失败时搜索仍成功，该条 `has_electronic=null`；成功时为布尔值。
6. **electricity 成功输出**：默认/env/config/flag 的 `source` 分别为 `default/env/config/flag`；`building/floor/room/surplus/price/remaining_kwh/total_consumed_kwh/meter_time/low_balance/threshold`；默认占位符为西土城学一楼 101 且 query floor 为 null；楼层从房间号推断。
7. **electricity buildings 成功**：默认 shahe、xitucheng/shahe 别名的请求 `areaid`；损坏 config 仍能成功列楼；`area` 回显为小写名称。
8. **electricity 上游错误**：`e != 0`、未登录 `m`、缺 `d.data`、错误类型；明确 auth/upstream/parse 退出码。
9. **notices 列表成功**：默认 `notice,news`，显式类型筛选，按时间倒序，`--count` 截断；列表缺 `newslist` 或没有任何有效项时的 parse 错误；登录页在一次明确过期后最多续期一次。
10. **notices 详情成功**：`title/url/author/time/content_text/attachments/images`；`--max-chars` 截断标记；正文缺失/标题缺失的 parse 错误；相对图片与附件 URL 解析。
11. **card 成功/错误**：`qkje/oddfare/mail/a` 映射，无效数字为 null；非 JSON、非对象、登录页；明确过期时最多续期一次。
12. **schema/pretty 一致性**：以上成功输出均带 `schema: "bupt.v1"`，`--pretty` 只改排版不改值。

## 旧版歧义（未在本轮决定）

1. **library 上游错误退出码**：Python `cmd_library_search` / `cmd_library_hold` / `cmd_library_ebook` 把 `LibraryError` 统一转成 `UsageError`（退出 2），包括上游返回 `result.code != 0` 和 hold 页面 404/500。Rust 合同的分层错误可能更适合 upstream 5；本轮没有写断言，交给整合者定。
2. **`notices show ""`**：Python 在建立会话前抛 `ValueError`，最终落成通用 upstream 5，而不是 usage 2。合同要求非法参数在联网前拒绝；空 URL 是否应成为 usage 尚未决定，本轮未写断言。
3. **`notices --count 0`**：Python 仍先建立会话（无凭据时 auth 3），然后客户端短路返回空列表。是否应允许无会话的空查询未定，本轮未测。
4. **损坏 config 与完整 flag 的关系**：Python `resolve_dorm` 无条件读取 `config.json`，因此即使 flag 已给全，损坏配置仍会让查询失败。本轮只验证「参数本身非法时先报参数错误」和「buildings 不读配置」，没有决定完整 flag 是否应绕过损坏配置。
5. **`electricity buildings` 是否需要登录**：Python 走 `_session(required=True)`，所以无凭据是 auth 3。若上游元数据实际可匿名读取，这是合同未写明的新行为；本轮按旧版断言 auth。
6. **card 数字容错**：Python 把无法解析的 `qkje/oddfare/mail` 静默变成 null。是否保留静默容错还是报 parse 错误，本轮未决定。
7. **notices 详情输出字段**：CLI 只输出 `title/url/author/time/content_text/content_truncated/attachments/images`，不输出 `content_html` 与 `blocks`。本轮按「保留能沿用的字段」记录，不扩字段。

## 验证记录

执行目录：`/Users/limoon/Documents/moon_cli_or_tool/.worktrees/bupt-rust/campus`

```sh
cargo fmt --check
cargo test --offline --test contract_campus --no-run
cargo test --offline --test contract_campus -- --test-threads=1
```

- `cargo fmt --check`：通过。共享 `tests/contract_support/mod.rs` 由整合者维护，本 lane 不加格式改动；`contract_campus.rs` 对 `mod contract_support` 使用 `#[rustfmt::skip]`，避免本 lane 改写共享文件。
- `cargo test --offline --test contract_campus --no-run`：编译通过。
- 红跑：29 个用例全部失败，退化为脚手架行为——进程 exit 78，stderr 是纯文本 `Rust CLI not implemented; test migration scaffold only`，因此每个用例在解析 JSON 时准确失败。没有真实联网、编译错误、`#[ignore]` 或 `#[should_panic]`。
