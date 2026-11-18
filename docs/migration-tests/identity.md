# 身份测试迁移清单（login / status / logout / diagnose）

Python 对照版本：`d7c35ef`。本清单记录测试迁移和第一批本地身份实现的验收状态。

## 范围

- 公开入口：`login`、`status`、`logout`、`diagnose`（Python 旧名 `probe`）。
- Python 基准：`tests/test_session_and_probe.py`、`tests/test_cli_errors.py` 中的鉴权/输出用例，以及 `src/bupt_cli/{session,credentials,probe,errors}.py` 的现有行为。
- Rust 用例：`tests/contract_identity.rs`。
- 夹具：`tests/fixtures/contracts/identity/`。全部是合成账号、Cookie、token 和本地状态；密码/令牌值均带 `TEST-ONLY-` 前缀，不含真实凭据或浏览器资料。
- 会话夹具按缓存版本区分：
  - v2 夹具同时带 `username` 和 `role`，表示归属元组 `(username, role)`；Cookie、UCloud token、移动教务 token 都只属于这个元组。
  - v1 夹具只有 `username`，没有 `role`。经验证学号后，只允许复用与身份无关的 CAS/UCloud 状态；v1 的移动教务 token 不能在没有角色归属时复用。
  - 需要可用移动教务 token 的用例使用 `session-undergraduate-mobile.json`（v2，本科生）；研究生夹具不携带可用的移动教务 token，避免角色矛盾。
- 未搬 Python 私有函数（`_Collector`、`_short`、`_ticket_from_response`、`_ucloud_headers`）及其内部结构；对应行为尽量从 CLI 边界验证，不能从边界验证的部分列在下面的待实现场景中。

## 初始红灯

在 `84ca24d` 脚手架和当时的测试修订上：

```sh
cargo test --offline --test contract_identity
```

当时的 21 个用例全部失败，失败原因一致：`src/main.rs` 仍以退出码 `78` 输出 `Rust CLI not implemented; test migration scaffold only`，没有实现任何公开入口。没有编译错误、网络访问、`#[ignore]` 或 `#[should_panic]`。当前实现新增了 6 个审查用例，后续凭据合同修订又新增 1 个，总数变为 28。

格式检查：

```sh
cargo fmt --check -- --config skip_children=true   # 本 lane 通过
```

共享 harness 的格式问题已在整合分支 `008c9f3` 修复。本 lane 不修改 `tests/contract_support/mod.rs`，只在本地用 `skip_children=true` 检查本 lane 的测试文件；整合后应以普通 `cargo fmt --check` 复跑。

## 第一批本地实现状态

`src/identity.rs` 已实现本地 `login` 参数校验、只读 `status`、会话归属判定和 `logout`。CAS/UCloud 协议切片已在 `src/identity/cas.rs` 用注入传输验证，但尚未接到生产命令、网络传输或在线 `diagnose`。

- 有效的 `login --user … --password … --role …` 在校验后返回明确的 pending 错误（退出码 `5`，`type: upstream`），不读取或写入账号状态。
- 当前 `login` 是非交互解析里程碑：缺少 `--user`/`--password` 会 usage 退出；最终登录仍必须恢复交互式提示。
- `status` 的 `user` 只输出 `user_id`、`user_name`、`real_name`；未知键忽略。`user_id` 只接受字符串或整数，两个名称字段只接受字符串；对象/数组等嵌套值直接省略。缓存中的 `mobile_jwgl.last_error` 不原样输出，统一映射为不含细节的安全诊断。
- 没有密码的已保存 profile 可以在 `status` 中显示学号，但不会激活任何缓存会话；没有 profile 时同样不激活。
- `locally_valid` 要求至少一个可用的 CAS Cookie：非空 domain、非空 name、字符串 value，且未明确过期。过期或格式不完整的 Cookie 只增加 `cookie_count`，不能单独让状态变成可用；缺失/不可解析的过期时间仍按未知处理。
- `diagnose --only cas` 在没有可用 CAS Cookie 时返回本地 skipped 报告；其他需要联网的组返回 pending，不写会话。
- `logout` 分别 unlink 自己拥有的 `credentials.json` 和 `session.json`，不重写打开的文件；`config.json` 和 `timetable.json` 保持原样。符号链接和非普通文件会被拒绝并报 auth 错误。
- 本 lane 的 28 个进程测试全部通过；另有 3 个 `identity` lib 单测覆盖共享 profile API；待实现场景没有以永远通过或伪造断言的形式保留。

## 共享本地 profile API（本轮）

`src/identity.rs` 提供 crate-internal、只读的 `load_profile(config_dir)`，返回 `Option<LocalProfile>`：

- `LocalProfile::username() -> &str`：只读取 `credentials.json` 的 `username` 字符串，trim 后非空才返回。
- `LocalProfile::role() -> Option<Role>`：只用同一文件的 `role` 字符串做类型化解析；缺失和非法角色都是 `None`。
- `LocalProfile::has_password() -> bool`：只报告 `credentials.json` 中是否有非空密码字符串，不保存或返回密码内容。
- `require_authenticated_profile(config_dir)`：在 `load_profile` 之上要求 username 和非空密码；缺失、不可读或不可解析统一返回 auth 退出码 `3`，不检查 role。

`LocalProfile` 不包含 password/cookie/token 字段，`Debug` 也只显示 username/role/has_password。这个 API 只读 `credentials.json`，不读取账号环境变量、会话或网络，也不写文件。

本轮同时迁移了调用方：`status`/`diagnose` 继续使用 `load_profile`；`academics`、`homework`、`campus` 的受保护命令改用共享鉴权入口。`academics` 只在需要选择本科/研究生数据源的操作里继续要求合法 role，`homework`/`campus` 不要求 role；匿名 `library` 路径仍不读取凭据。

新增 3 个 lib 单测：profile 规范化/类型化角色/密码存在性及 Debug 脱敏、无密码和缺失/非法角色、缺失 profile 文件。进程级用例另验证环境变量不能补充或覆盖 JSON 账号、密码存在性和 role。

## 凭据读取统一（本轮）

| 调用方 | 读取入口 | role 语义 | 缺失/异常 |
| --- | --- | --- | --- |
| `identity` `status` / `diagnose` | `load_profile` | 可为 `None`；`status` 仍可显示学号但不激活缓存 | 文件缺失 -> `None`；读取/解析失败 -> auth |
| `academics` `schedule` / `exams` / `scores` / `classrooms` | `require_authenticated_profile` | 这些操作需要 role 选择/校验数据源；缺失或非法 role -> usage | 无 username、无密码、读取/解析失败 -> auth |
| `homework` `courses` / `homework` | `require_authenticated_profile` | 不读取 role | 无 username、无密码、读取/解析失败 -> auth |
| `campus` 受保护命令 | `require_authenticated_profile` | 不读取 role | 无 username、无密码、读取/解析失败 -> auth |

统一后的边界：

1. `credentials.json` 是 username、密码存在性和 role 的唯一持久化来源；`BUPT_USERNAME`、`BUPT_PASSWORD`、`BUPT_ROLE` 不再读取，也不作为提示或回退。
2. 匿名命令不读取凭据；参数校验仍先于凭据读取。
3. role 缺失或非法只在确实需要 role 的 `academics` 操作中报 usage；其他使用方不因 role 阻止仅需登录的命令。
4. `status` 的只读语义和 session 归属规则不变；无密码 profile 仍然只显示学号，不激活缓存。

## CAS/UCloud 协议切片（本轮）

`src/identity/cas.rs` 是从 Python `auth.py` 抽出的单个平铺子模块，只服务测试和后续接线，当前没有接到任何生产命令、传输工厂或会话持久化：

- `cas_login` 先 GET CAS 登录页并提取 `execution`，然后只提交一次账号密码表单。所有重定向都校验 URL、host 和 path；POST 不跟随 307/308，返回明确的 auth 错误，避免重放密码。其余重定向只以 GET 跟随，目标必须是 HTTPS 的 `auth.bupt.edu.cn/authserver/*` 或 `ucloud.bupt.edu.cn/uclass*`；端口、userinfo、非 TLS、外部 host、路径穿越和超长重定向链都拒绝。TLS 证书校验由最终传输实现负责；本切片只保证请求 URL 和重定向目标不会离开 HTTPS 白名单。
- CAS 表单最终页必须是 2xx，且不能仍是 `/authserver/login` 登录页。随后必须再走普通 CAS service-ticket 校验请求：只有 301/302/303/307/308 这类明确重定向状态，且 `Location` 指向允许的 UCloud path 并携带 ticket，才算登录成功。2xx 即使带 `Location` 也不算；空页、无关页、登录页、300/304、缺失 ticket 或无 `Location` 的 3xx 都 fail closed。
- `CasLogin` 的字段私有，只有拿到 service ticket 才能构造；最终 URL 不再暴露，ticket 不进入 `Debug`。当前切片不写任何文件；后续 `login` 接线必须从该值取得已确认身份后再写 `credentials.json`/`session.json`，失败路径不得写状态。
- `ucloud_token` 是惰性的：CAS 登录本身不请求 OAuth token。登录拿到的 service ticket 在第一次真正需要 token 时使用；之后 ticket 已消费或 token 过期时，再通过同一 CAS service-ticket 校验请求取新 ticket。token 只缓存在内存，并按既有 `UCLOUD_EXPIRY_BUFFER_SECONDS`/JWT `exp` 判断是否过期；ticket 交换不跟随进入 UCloud。后续读取使用 `Blade-Auth`、`Tenant-Id: 000000` 和既有 UCloud 客户端 Basic 授权头，不把 CAS Cookie 或密码带到 UCloud API。
- 错误按全局合同分层：401/403 是 auth `3`；其他 HTTP 4xx/5xx（包括限流 429）是 upstream `5`，其中 429 只提示等待，其他提示服务响应异常，均不提示重试密码；连接/传输失败是 network `4`。缺少 execution、畸形页面/JSON、缺失 `Location` 是 upstream `5`；没有 ticket、ticket 重定向不合法或缺少 `access_token` 是 auth `3`。错误消息不回显密码、Cookie、ticket 或 token。
- 15 个 lib 单测使用 `Scripted` 和 `TEST-ONLY-` 合成值：单次密码 POST、service ticket 正向证据、显式重定向状态（301/302/303/307/308）、允许 302 后空/无关 200 无 ticket 的失败关闭、200 带 `Location` 不得当 ticket、307/308 拒绝、非 TLS/外部 host 拒绝、HTTP 4xx/5xx 与连接失败分层（含 429）、缺失 execution、惰性 token 与缓存复用、过期 token 不重用、UCloud 读取路径校验、ticket 重定向校验、token 响应成功条件。当前 lib 共 56 个测试；`contract_identity` 仍是 28 个进程测试。
- 未实现：真实传输接线、账号/会话写入、自动续期/登录次数上限、在线 `diagnose`、Windows ACL、真实校园网验收。以上仍保留在下面的待实现场景中。

**真实接线门槛**：URL 与 Cookie 已改用 `url` 和 `cookie_store`。保留明确 HTTPS 主机和路径限制，拒绝显式端口和编码路径穿越；CAS Cookie 只从 CAS 接收、只发往 CAS。新增两个合成用例覆盖 Cookie 默认路径、过期、域隔离及 URL 规范化。该协议仍仅编译进测试；还需接入受限传输、会话持久化与业务命令后，再用独立状态实际运行 `login`、`courses`、`homework --undone`。匿名 CAS 登录页可达不算认证通过。

## 用例映射

| Rust 用例 | Python 依据 | 覆盖的合同行为 |
| --- | --- | --- |
| `login_requires_explicit_role_and_rejects_unknown_role_before_writing` | `commands.py::cmd_login`、`credentials.normalize_role`；旧 CLI 的交互式身份提示 | `login` 必须登记明确的本科/研究生身份；缺角色或非法角色在写凭据、建会话前以 usage 退出。 |
| `status_check_flag_is_removed_and_does_not_touch_state` | 合同对 Python `status --check` 的移除；`test_cli_errors.py` 的 usage 输出 | `status --check` 作为已移除参数在读取/联网/写状态前以 usage 拒绝；配置、课表、会话保持不变。 |
| `login_jwgl_flag_is_removed_and_does_not_touch_state` | 合同对 Python `login --jwgl` 的移除 | `login --jwgl` 作为已移除参数在验证/写凭据前以 usage 拒绝；旧凭据和会话不被替换，尝试密码不泄露。 |
| `invalid_login_arguments_keep_last_registered_credentials_and_session` | `commands.py::cmd_login` 先 `save_credentials` 再验证的旧顺序 | **非法参数**不得覆盖上一次有效登记和会话。这里只覆盖联网前的角色校验失败，不冒充延迟 CAS 拒绝；后者见待实现场景。 |
| `changed_account_does_not_reuse_previous_owners_session` | `test_session_cache_username_mismatch` | 学号变化时不把旧会话当成本账号的会话；旧所有者账号和 Cookie 材料不进入报告；配置中的当前合成学号可以正常显示。 |
| `saved_credentials_ignore_conflicting_account_environment` | `test_credentials_env_overrides_config` 的反向合同变化 | JSON 里的 username/password/role 全部优先；冲突的账号、密码和 role 环境变量不能替换本地 profile，也不能让匹配缓存失效。 |
| `environment_only_account_is_not_a_profile` | Python 环境变量优先行为的有意变更 | 只有账号环境变量、没有 `credentials.json` 时，`status` 不把环境变量当 profile，也不激活缓存；只读且不泄露合成账号/密码。 |
| `same_username_different_role_does_not_reuse_v2_cache` | 合同“身份切换不复用另一所有者的状态” | 同一学号、不同角色（本科生凭据 + 研究生 v2 会话）不满足归属元组；CAS、UCloud、移动教务状态都不得复用。 |
| `legacy_username_only_cache_reuses_cas_ucloud_but_not_mobile` | 合同“只复用归属可验证且兼容的旧会话”；Python 旧缓存没有角色字段 | v1 学号匹配但缺少角色时，只复用 CAS/UCloud；即使缓存里有移动教务 token，也不能在没有角色归属时复用。 |
| `v2_undergraduate_cache_reuses_verified_mobile_token` | v2 归属元组；`probe.py` 的移动教务缓存 token 规则 | 本科生 v2 会话带角色和可用移动教务 token 时可以显示该 token 存在且未过期；`status` 不写回、不改登录计数。 |
| `unowned_session_cache_is_not_adopted` | `BuptSession.__init__` 的缓存读取、`test_session_cache_username_mismatch` | 旧会话缺少可验证的学号归属时不得复用，即使凭据本身可读。 |
| `status_without_credentials_succeeds_without_creating_state` | `test_status_works_without_any_credentials` | 新机器没有凭据时 `status` 仍成功；只读本机状态，不创建凭据或会话文件。 |
| `missing_profile_does_not_activate_cached_session` | 合同“只复用归属可验证且兼容的旧会话” | 没有 profile、但缓存里有完整会话时，`status` 不激活缓存；只读且不泄露缓存秘密。 |
| `passwordless_profile_is_named_but_does_not_activate_cached_session` | 合同“只复用归属可验证且兼容的旧会话” | 无密码的已保存 profile 可以在 `status` 中显示学号，但不能激活缓存会话；配置和缓存都不变。 |
| `status_allowlists_user_fields_and_suppresses_cached_diagnostics` | 合同对 `status` 只读和秘密边界的要求 | `user` 只允许 `user_id`/`user_name`/`real_name`，未知键中的合成秘密不输出；缓存 JWGL 错误不含原始自由文本，而是安全诊断。 |
| `status_public_user_omits_nested_public_values` | 合同对缓存 `user` 对象的最小输出要求 | 公共字段下放对象或数组时直接省略；`user_id` 的整数标量保留，`user_name`/`real_name` 的合法字符串保留；嵌套合成秘密不进入输出。 |
| `status_reads_cached_state_without_mutation_or_secret_leak` | `test_session_file_permissions_and_status_redaction` | `status` 读取合法缓存但不改写配置/缓存；可以显示配置中的当前学号，但密码、Cookie、UCloud token、移动教务 token 不泄露；`cas_logins_total` 不变。 |
| `status_pretty_changes_format_not_values` | `test_pretty_flag_after_subcommand`、`test_success_envelope_has_ok` | `--pretty` 只改排版；成功输出为单个 `schema: "bupt.v1"` 文档。 |
| `status_reports_explicit_expiry_without_refreshing` | `session.py::BuptSession.status`、`test_session_file_permissions_and_status_redaction` | 只有缓存中明确的过期时间才标记 expired；`status` 不自动续期、不写回、不增加 CAS 登录计数。 |
| `status_missing_expiry_does_not_expire_or_relogin` | `auth.py::is_token_expired`（无 `exp` 时返回 false） | 缺少过期信息不等于过期，也不意味着要重新登录；`status` 不续期、不写回、不改变登录计数。 |
| `status_tolerates_non_numeric_cookie_expiry` | `test_status_tolerates_non_numeric_cookie_expiry` | 单个 Cookie 的过期值不可解析时，其他 Cookie 仍参与计数和最早过期时间计算；不可解析的过期值按未知处理，不单独判定过期。 |
| `locally_valid_requires_a_usable_cookie_and_keeps_unknown_expiry` | 合同对 `status` 本地可用性和过期语义的要求 | 只有 Cookie 槽位不够：过期、缺少 domain/name/value 或类型不对的 Cookie 不能使 `locally_valid=true`；缺失过期时间仍是未知且可用；`diagnose` 同样不把这种槽位当成可用 CAS 会话。 |
| `graduate_status_drops_stale_mobile_jwgl_failure_but_undergraduate_keeps_it` | `test_graduate_status_drops_stale_mobile_jwgl_error`、`test_undergraduate_status_keeps_mobile_jwgl_error` | 研究生身份不展示本科教务的陈旧失败；本科生仍能看到一个安全、非原始的错误诊断；`status` 只在内存中处理，不写回。两类夹具分别带匹配的角色。 |
| `unreadable_credentials_are_auth_error_before_network_or_write` | `credentials.py::_read_config` 的 `AuthError`、`test_error_envelope_exit_codes` | 凭据文件不可解析时走 auth 退出码 `3`，错误流是 `bupt.v1` 信封，不联网、不写会话、不泄露文件内容。 |
| `logout_clears_password_and_session_but_keeps_timetable_and_config` | `test_delete_session`；合同对旧行为的变更 | `logout` 分别删除自己拥有的凭据和会话文件；`config.json` 和 `timetable.json` 原样保留；重复执行幂等（`removed=false`）。 |
| `logout_refuses_to_follow_credentials_symlink` | 合同对本地凭据所有权的边界 | 当 `credentials.json` 是指向临时夹具的符号链接时，`logout` 安全拒绝，不跟随链接、不改目标文件、也不删除会话或配置。 |
| `diagnose_skips_authenticated_checks_without_session_and_does_not_write` | `test_probe_runs_without_credentials`、`probe.py` 的 CAS 跳过逻辑 | 没有可用 CAS 会话时，`diagnose --only cas` 只报告 skipped；不提交密码、不创建会话、不改本地状态，且只运行请求的组。 |
| `diagnose_unknown_group_is_usage_before_io` | `test_probe_unknown_group_is_usage_error` | 未知 `--only` 组在联网和写状态前以 usage 退出；已有配置、课表、会话不变。 |

## 与 Python 的有意差异

- Python 的 `probe` 改名为 `diagnose`；合同不保留 `probe` 别名。
- Python `status --check` 会联网；合同删除了该参数，联网诊断归到 `diagnose`。本修订直接验证它被 usage 拒绝，没有猜测兼容行为。
- Python `login --jwgl` 会尝试移动教务登录；合同删除了该参数。本修订直接验证它被 usage 拒绝。
- Python `probe._save_probe_state()` 会在探测过程中写 `session.json`；Rust `diagnose` 明确不写本地会话，测试用前后快照约束。
- Python `login` 在验证前保存新凭据；合同要求失败时保留上一次有效登记和会话。当前实现还在 pending 前，后续接线必须保持这个顺序。
- Python `logout` 只删 `session.json`，不清密码；合同要求同时忘掉本地凭据和会话，但保留课表和设置。Rust 直接 unlink 两个自有文件，不重写或截断；符号链接/非普通文件拒绝。
- Python `status.user` 原样返回缓存对象；Rust 只允许 `user_id`、`user_name`、`real_name`，且只接受字符串/整数 ID 和字符串名称，嵌套值不输出。Python `mobile_jwgl.last_error` 原样返回；Rust 映射为安全诊断，不输出缓存自由文本。
- Python 的凭据读取允许环境变量覆盖 `credentials.json`；Rust 合同改为只读 JSON，账号环境变量不再补充或覆盖任何字段。相关进程用例验证 env-only 被忽略、冲突 env 不改变本地账号和 role。
- Python 只要缓存有 Cookie 且学号相同就可能复用；Rust 要求 profile 含密码、v2 归属元组 `(username, role)`，并要求至少一个未明确过期且 name/value 完整的 CAS Cookie。v1 只复用身份无关的 CAS/UCloud 状态，不复用无法确认角色的移动教务状态。
- 当前 Rust `login` 只实现非交互参数解析；Python 在缺参数时提示输入。最终登录仍必须恢复交互式提示；本轮只把它记录为里程碑，不把非交互行为当作最终合同。
- 成功/错误输出统一带 `schema: "bupt.v1"`；这是迁移合同的新增外壳。

## 待实现场景与所需测试缝

以下场景不能在当前共享 harness 下可靠验证。当前没有对应的失败测试；它们需要整合者先确认最小测试缝，不能用永远通过的空断言或猜测内部 API 代替。

1. **延迟 CAS 拒绝时保留旧状态**（与非法参数用例分开）。
   当前 `src/identity/cas.rs` 已能用 `Scripted` 证明 CAS 失败/拒绝的请求形状，但 `login` 尚未接入该切片。接线后仍需让 `login` 在参数合法后得到确定的 auth/network 失败，并断言旧 `credentials.json`、旧 `session.json` 和课表/配置均不变。当前 `invalid_login_arguments_...` 只覆盖联网前拒绝，不能代替本场景。

2. **同一学号改密码后不能用旧会话证明新密码**。
   需要凭据版本/指纹缝，或传输层注入：旧会话存在、同一学号提交新密码时，必须发生一次真实凭据验证，不能直接返回旧会话成功。仅靠本地文件无法知道旧会话是用哪一代密码建立的。

3. **成功登录的凭据存储与续期写回**。
   需要成功登录的模拟传输缝：验证 `credentials.json`/`session.json` 以用户私有权限创建、未知键合并保留、密码不出现在输出；macOS `0600` 之外还需要 Windows 用户 ACL 的等价缝。当前用例只验证已存在文件的读取/删除边界，不验证创建。

4. **续期有界且只由明确过期触发**。
   需要脚本化传输序列和请求/登录计数观测：缓存票据失效时最多一次自动续期；达到 `BUPT_MAX_CAS_LOGINS` 预算后不再登录；暂时断网、密码拒绝、HTTP 401/403、限流（429）不得被当作过期信号触发续期。当前只能验证 `status` 不续期、不改变登录计数、缺失过期时间不触发重登。

5. **`diagnose` 的公共可达性检查和移动教务无 token 在线路径**。
   Python 的 `mobile_jwgl` 组包含一个未认证的 GET（移动教务登录端点可达性），因此不能把所有移动教务行都断言为 skipped，也不能在离线共享 harness 里真的发出该请求，否则会意外联网。
   需要传输注入缝：使用 `session-undergraduate-no-mobile.json`（v2，本科生，无移动教务 token）运行 `diagnose --only mobile_jwgl`，允许未认证 GET 行返回注入的 ok/fail，同时断言需要 token 的移动教务行 skipped；用请求捕获做正向断言：任何请求行都没有密码字段/密码头，凭据和会话文件前后不变。当前只保留离线安全的 `--only cas` 无会话跳过用例。

6. **`diagnose` 的请求体/刷新行为**。
   需要传输 spy 捕获请求：确认任何组都不提交密码；UCloud token 刷新只走 ticket 交换且不写 `session.json`；移动教务只用缓存 token。当前用无会话跳过避免联网，只能验证跳过和磁盘不变。

7. **上游回声中的秘密脱敏**。
   需要可注入错误消息的故障缝，让上游错误包含合成密码/token，断言 `[REDACTED]` 或完全移除。当前用例只断言本地已知输出不包含夹具秘密。

8. **Windows 用户访问边界**。
   Unix 用例只检查 `0600`；Windows 需要 ACL 等价验证（仅属主可读），放在 Windows runner 上，不能在 macOS 上假装通过。

9. **交互式登录提示**。
   最终 `login` 必须恢复缺参数时的交互式账号/密码提示；当前非交互参数解析只是本地里程碑，不能作为最终合同。
