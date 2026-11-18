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

在 `84ca24d` 脚手架和本修订后的测试上：

```sh
cargo test --offline --test contract_identity
```

21 个用例全部失败，失败原因一致：`src/main.rs` 仍以退出码 `78` 输出 `Rust CLI not implemented; test migration scaffold only`，没有实现任何公开入口。没有编译错误、网络访问、`#[ignore]` 或 `#[should_panic]`。

格式检查：

```sh
cargo fmt --check -- --config skip_children=true   # 本 lane 通过
```

共享 harness 的格式问题已在整合分支 `008c9f3` 修复。本 lane 不修改 `tests/contract_support/mod.rs`，只在本地用 `skip_children=true` 检查本 lane 的测试文件；整合后应以普通 `cargo fmt --check` 复跑。

## 第一批本地实现状态

`src/identity.rs` 已实现本地 `login` 参数校验、只读 `status`、会话归属判定和 `logout`。未接 CAS 验证、网络传输或在线 `diagnose`。

- 有效的 `login --user … --password … --role …` 在校验后返回明确的 pending 错误（退出码 `5`，`type: upstream`），不读取或写入账号状态。
- `diagnose --only cas` 在没有缓存 CAS Cookie 时返回本地 skipped 报告；其他需要联网的组返回 pending，不写会话。
- 本 lane 的 21 个进程测试全部通过；待实现场景没有以永远通过或伪造断言的形式保留。

## 用例映射

| Rust 用例 | Python 依据 | 覆盖的合同行为 |
| --- | --- | --- |
| `login_requires_explicit_role_and_rejects_unknown_role_before_writing` | `commands.py::cmd_login`、`credentials.normalize_role`；旧 CLI 的交互式身份提示 | `login` 必须登记明确的本科/研究生身份；缺角色或非法角色在写凭据、建会话前以 usage 退出。 |
| `status_check_flag_is_removed_and_does_not_touch_state` | 合同对 Python `status --check` 的移除；`test_cli_errors.py` 的 usage 输出 | `status --check` 作为已移除参数在读取/联网/写状态前以 usage 拒绝；配置、课表、会话保持不变。 |
| `login_jwgl_flag_is_removed_and_does_not_touch_state` | 合同对 Python `login --jwgl` 的移除 | `login --jwgl` 作为已移除参数在验证/写凭据前以 usage 拒绝；旧凭据和会话不被替换，尝试密码不泄露。 |
| `invalid_login_arguments_keep_last_registered_credentials_and_session` | `commands.py::cmd_login` 先 `save_credentials` 再验证的旧顺序 | **非法参数**不得覆盖上一次有效登记和会话。这里只覆盖联网前的角色校验失败，不冒充延迟 CAS 拒绝；后者见待实现场景。 |
| `changed_account_does_not_reuse_previous_owners_session` | `test_session_cache_username_mismatch` | 学号变化时不把旧会话当成本账号的会话；旧所有者账号和 Cookie 材料不进入报告；配置中的当前合成学号可以正常显示。 |
| `environment_account_override_does_not_reuse_configured_owners_session` | `test_credentials_env_overrides_config` | `BUPT_USERNAME`/`BUPT_PASSWORD` 覆盖配置文件时，会话归属按最终账号判断；旧所有者材料隐藏，当前环境账号可以显示。 |
| `same_username_different_role_does_not_reuse_v2_cache` | 合同“身份切换不复用另一所有者的状态” | 同一学号、不同角色（本科生凭据 + 研究生 v2 会话）不满足归属元组；CAS、UCloud、移动教务状态都不得复用。 |
| `legacy_username_only_cache_reuses_cas_ucloud_but_not_mobile` | 合同“只复用归属可验证且兼容的旧会话”；Python 旧缓存没有角色字段 | v1 学号匹配但缺少角色时，只复用 CAS/UCloud；即使缓存里有移动教务 token，也不能在没有角色归属时复用。 |
| `v2_undergraduate_cache_reuses_verified_mobile_token` | v2 归属元组；`probe.py` 的移动教务缓存 token 规则 | 本科生 v2 会话带角色和可用移动教务 token 时可以显示该 token 存在且未过期；`status` 不写回、不改登录计数。 |
| `unowned_session_cache_is_not_adopted` | `BuptSession.__init__` 的缓存读取、`test_session_cache_username_mismatch` | 旧会话缺少可验证的学号归属时不得复用，即使凭据本身可读。 |
| `status_without_credentials_succeeds_without_creating_state` | `test_status_works_without_any_credentials` | 新机器没有凭据时 `status` 仍成功；只读本机状态，不创建凭据或会话文件。 |
| `status_reads_cached_state_without_mutation_or_secret_leak` | `test_session_file_permissions_and_status_redaction` | `status` 读取合法缓存但不改写配置/缓存；可以显示配置中的当前学号，但密码、Cookie、UCloud token、移动教务 token 不泄露；`cas_logins_total` 不变。 |
| `status_pretty_changes_format_not_values` | `test_pretty_flag_after_subcommand`、`test_success_envelope_has_ok` | `--pretty` 只改排版；成功输出为单个 `schema: "bupt.v1"` 文档。 |
| `status_reports_explicit_expiry_without_refreshing` | `session.py::BuptSession.status`、`test_session_file_permissions_and_status_redaction` | 只有缓存中明确的过期时间才标记 expired；`status` 不自动续期、不写回、不增加 CAS 登录计数。 |
| `status_missing_expiry_does_not_expire_or_relogin` | `auth.py::is_token_expired`（无 `exp` 时返回 false） | 缺少过期信息不等于过期，也不意味着要重新登录；`status` 不续期、不写回、不改变登录计数。 |
| `status_tolerates_non_numeric_cookie_expiry` | `test_status_tolerates_non_numeric_cookie_expiry` | 单个 Cookie 的过期值不可解析时，其他 Cookie 仍参与计数和最早过期时间计算。 |
| `graduate_status_drops_stale_mobile_jwgl_failure_but_undergraduate_keeps_it` | `test_graduate_status_drops_stale_mobile_jwgl_error`、`test_undergraduate_status_keeps_mobile_jwgl_error` | 研究生身份不展示本科教务的陈旧失败；本科生仍能看到该失败；`status` 只在内存中处理，不写回。两类夹具分别带匹配的角色。 |
| `unreadable_credentials_are_auth_error_before_network_or_write` | `credentials.py::_read_config` 的 `AuthError`、`test_error_envelope_exit_codes` | 凭据文件不可解析时走 auth 退出码 `3`，错误流是 `bupt.v1` 信封，不联网、不写会话、不泄露文件内容。 |
| `logout_clears_password_and_session_but_keeps_timetable_and_config` | `test_delete_session`；合同对旧行为的变更 | `logout` 删除会话并清掉本地密码；`config.json` 和 `timetable.json` 原样保留；重复执行幂等（`removed=false`），剩余凭据文件仍为用户私有。 |
| `diagnose_skips_authenticated_checks_without_session_and_does_not_write` | `test_probe_runs_without_credentials`、`probe.py` 的 CAS 跳过逻辑 | 没有可用 CAS 会话时，`diagnose --only cas` 只报告 skipped；不提交密码、不创建会话、不改本地状态，且只运行请求的组。 |
| `diagnose_unknown_group_is_usage_before_io` | `test_probe_unknown_group_is_usage_error` | 未知 `--only` 组在联网和写状态前以 usage 退出；已有配置、课表、会话不变。 |

## 与 Python 的有意差异

- Python 的 `probe` 改名为 `diagnose`；合同不保留 `probe` 别名。
- Python `status --check` 会联网；合同删除了该参数，联网诊断归到 `diagnose`。本修订直接验证它被 usage 拒绝，没有猜测兼容行为。
- Python `login --jwgl` 会尝试移动教务登录；合同删除了该参数。本修订直接验证它被 usage 拒绝。
- Python `probe._save_probe_state()` 会在探测过程中写 `session.json`；Rust `diagnose` 明确不写本地会话，测试用前后快照约束。
- Python `logout` 只删 `session.json`，不清密码；合同要求同时清掉本地密码并保留课表和设置。
- Python `login` 在验证前保存新凭据；合同要求失败时保留上一次有效登记和会话。
- Python 只要缓存有 Cookie 且学号相同就可能复用；Rust v2 使用归属元组 `(username, role)`。v1 只复用身份无关的 CAS/UCloud 状态，不复用无法确认角色的移动教务状态。
- 成功/错误输出统一带 `schema: "bupt.v1"`；这是迁移合同的新增外壳。

## 待实现场景与所需测试缝

以下场景不能在当前共享 harness 下可靠验证。当前没有对应的失败测试；它们需要整合者先确认最小测试缝，不能用永远通过的空断言或猜测内部 API 代替。

1. **延迟 CAS 拒绝时保留旧状态**（与非法参数用例分开）。
   需要可注入的传输层或脚本化 CAS 响应，让 `login` 在参数合法后得到确定的 auth/network 失败；断言旧 `credentials.json`、旧 `session.json` 和课表/配置均不变。当前 `invalid_login_arguments_...` 只覆盖联网前拒绝，不能代替本场景。

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
