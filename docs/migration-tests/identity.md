# 身份测试迁移清单（login / status / logout / diagnose）

Python 对照版本：`d7c35ef`。本清单只记录测试阶段，不包含实现。

## 范围

- 公开入口：`login`、`status`、`logout`、`diagnose`（Python 旧名 `probe`）。
- Python 基准：`tests/test_session_and_probe.py`、`tests/test_cli_errors.py` 中的鉴权/输出用例，以及 `src/bupt_cli/{session,credentials,probe,errors}.py` 的现有行为。
- Rust 用例：`tests/contract_identity.rs`。
- 夹具：`tests/fixtures/contracts/identity/`。全部是合成账号、Cookie、token 和本地状态；密码/令牌值均带 `TEST-ONLY-` 前缀，不含真实凭据或浏览器资料。
- 未搬 Python 私有函数（`_Collector`、`_short`、`_ticket_from_response`、`_ucloud_headers`）及其内部结构；对应行为尽量从 CLI 边界验证，不能从边界验证的部分列在下面的待实现场景中。

## 初始红灯

在 `84ca24d` 的脚手架和本测试提交上：

```sh
cargo test --offline --test contract_identity
```

17 个用例全部失败，失败原因一致：`src/main.rs` 仍以退出码 `78` 输出 `Rust CLI not implemented; test migration scaffold only`，没有实现任何公开入口。没有编译错误、网络访问、`#[ignore]` 或 `#[should_panic]`。

格式检查：

```sh
cargo fmt --check -- --config skip_children=true   # 通过
```

直接运行 `cargo fmt --check` 仍会报告脚手架自带的 `tests/contract_support/mod.rs` 格式差异；该文件是共享 harness，本 lane 按约定不改。

## 用例映射

| Rust 用例 | Python 依据 | 覆盖的合同行为 |
| --- | --- | --- |
| `login_requires_explicit_role_and_rejects_unknown_role_before_writing` | `commands.py::cmd_login`、`credentials.normalize_role`；旧 CLI 的交互式身份提示 | `login` 必须登记明确的本科/研究生身份；缺角色或非法角色在写凭据、建会话前以 usage 退出。 |
| `failed_login_validation_keeps_last_registered_credentials_and_session` | `commands.py::cmd_login` 先 `save_credentials` 再验证的旧顺序 | 失败的登录尝试不得覆盖上一次有效登记和会话；本用例用联网前的参数校验失败验证，完整鉴权失败见待实现场景。 |
| `changed_account_does_not_reuse_previous_owners_session` | `test_session_cache_username_mismatch` | 学号变化时不把旧会话当成本账号的会话；`exists=false`、`cache_username_mismatch=true`、旧 Cookie 不进入报告。 |
| `environment_account_override_does_not_reuse_configured_owners_session` | `test_credentials_env_overrides_config` | `BUPT_USERNAME`/`BUPT_PASSWORD` 覆盖配置文件时，会话归属按最终账号判断。 |
| `unowned_session_cache_is_not_adopted` | `BuptSession.__init__` 的缓存读取、`test_session_cache_username_mismatch` | 旧会话缺少可验证的学号归属时不得复用，即使凭据本身可读。 |
| `status_without_credentials_succeeds_without_creating_state` | `test_status_works_without_any_credentials` | 新机器没有凭据时 `status` 仍成功；只读本机状态，不创建凭据或会话文件。 |
| `status_reads_cached_state_without_mutation_or_secret_leak` | `test_session_file_permissions_and_status_redaction` | `status` 读取合法缓存但不改写配置/缓存；密码、Cookie、UCloud token、移动教务 token、学号不泄露；`cas_logins_total` 不变。 |
| `status_pretty_changes_format_not_values` | `test_pretty_flag_after_subcommand`、`test_success_envelope_has_ok` | `--pretty` 只改排版；成功输出为单个 `schema: "bupt.v1"` 文档。 |
| `status_reports_explicit_expiry_without_refreshing` | `session.py::BuptSession.status`、`test_session_file_permissions_and_status_redaction` | 只有缓存中明确的过期时间才标记 expired；`status` 不自动续期、不写回、不增加 CAS 登录计数。 |
| `status_treats_missing_expiry_as_unknown_not_expired` | `auth.py::is_token_expired`（无 `exp` 时返回 false） | 缺少过期信息不等于过期；不得为未知状态触发续期或写回。 |
| `status_tolerates_non_numeric_cookie_expiry` | `test_status_tolerates_non_numeric_cookie_expiry` | 单个 Cookie 的过期值不可解析时，其他 Cookie 仍参与计数和最早过期时间计算。 |
| `graduate_status_drops_stale_mobile_jwgl_failure_but_undergraduate_keeps_it` | `test_graduate_status_drops_stale_mobile_jwgl_error`、`test_undergraduate_status_keeps_mobile_jwgl_error` | 研究生身份不展示本科教务的陈旧失败；本科生仍能看到该失败；`status` 只在内存中处理，不写回。 |
| `unreadable_credentials_are_auth_error_before_network_or_write` | `credentials.py::_read_config` 的 `AuthError`、`test_error_envelope_exit_codes` | 凭据文件不可解析时走 auth 退出码 `3`，错误流是 `bupt.v1` 信封，不联网、不写会话、不泄露文件内容。 |
| `logout_clears_password_and_session_but_keeps_timetable_and_config` | `test_delete_session`；合同对旧行为的变更 | `logout` 删除会话并清掉本地密码；`config.json` 和 `timetable.json` 原样保留；重复执行幂等（`removed=false`），剩余凭据文件仍为用户私有。 |
| `diagnose_skips_authenticated_checks_without_session_and_does_not_write` | `test_probe_runs_without_credentials`、`probe.py` 的 CAS 跳过逻辑 | 没有可用 CAS 会话时，`diagnose --only cas` 只报告 skipped；不提交密码、不创建会话、不改本地状态，且只运行请求的组。 |
| `diagnose_does_not_login_mobile_with_cached_cas_session` | `probe.py`“mobile JWGL 只用缓存 token”；`test_graduate_mobile_login_is_never_attempted` | 有 CAS 会话但没有移动教务 token 时，`diagnose --only mobile_jwgl` 全部 skipped，不提交本科教务密码、不写回会话。 |
| `diagnose_unknown_group_is_usage_before_io` | `test_probe_unknown_group_is_usage_error` | 未知 `--only` 组在联网和写状态前以 usage 退出；已有配置、课表、会话不变。 |

## 与 Python 的有意差异

- Python 的 `probe` 改名为 `diagnose`；合同不保留 `probe` 别名。
- Python `probe._save_probe_state()` 会在探测过程中写 `session.json`；Rust `diagnose` 明确不写本地会话，测试用前后快照约束。
- Python `status --check` 会联网；合同把联网诊断归到 `diagnose`，`status` 只读本机状态。本 lane 没有为 `--check` 猜参数行为，见待实现场景。
- Python `logout` 只删 `session.json`，不清密码；合同要求同时清掉本地密码并保留课表和设置。
- Python `login` 在验证前保存新凭据；合同要求失败时保留上一次有效登记和会话。
- Python 只要缓存有 Cookie 且学号相同就可能复用；合同要求归属可验证且兼容，身份/学号变化不复用。
- 成功/错误输出统一带 `schema: "bupt.v1"`；这是迁移合同的新增外壳。

## 待实现场景与所需测试缝

以下场景不能在当前共享 harness 下可靠验证。没有用永远通过的空断言或猜测内部 API 代替；需要整合者先确认最小测试缝。

1. **完整登录鉴权失败保留旧状态**（对应 `failed_login_validation_keeps_last_registered_credentials_and_session` 的未覆盖部分）。
   需要可注入的传输层或脚本化 CAS 响应，让 `login` 得到确定的 auth/network 失败；断言旧 `credentials.json`、旧 `session.json` 和课表/配置均不变。当前只能用联网前的参数校验失败覆盖“失败不写”。

2. **同一学号改密码后不能用旧会话证明新密码**。
   需要凭据版本/指纹缝，或传输层注入：旧会话存在、同一学号提交新密码时，必须发生一次真实凭据验证，不能直接返回旧会话成功。仅靠本地文件无法知道旧会话是用哪一代密码建立的。

3. **身份切换不复用另一所有者的状态**。
   需要先确定会话文件中角色归属字段的兼容格式（Python 缓存没有该字段）。确定后用同一学号、不同 `role` 的旧会话验证不复用；当前只覆盖学号变化和缺失学号。

4. **成功登录的凭据存储与续期写回**。
   需要成功登录的模拟传输缝：验证 `credentials.json`/`session.json` 以用户私有权限创建、未知键合并保留、密码不出现在输出；macOS `0600` 之外还需要 Windows 用户 ACL 的等价缝。当前用例只验证已存在文件的读取/删除边界，不验证创建。

5. **续期有界且只由明确过期触发**。
   需要脚本化传输序列和请求/登录计数观测：缓存票据失效时最多一次自动续期；达到 `BUPT_MAX_CAS_LOGINS` 预算后不再登录；暂时断网、密码拒绝、HTTP 401/403、限流（429）不得被当作过期信号触发续期。当前只能验证 `status` 不续期、不改变登录计数。

6. **`diagnose` 的请求体/刷新行为**。
   需要传输 spy 捕获请求：确认任何组都不提交密码；UCloud token 刷新只走 ticket 交换且不写 `session.json`；移动教务只用缓存 token。当前用“没有会话/没有移动 token 时跳过”避免联网，只能验证跳过和磁盘不变。

7. **上游回声中的秘密脱敏**。
   需要可注入错误消息的故障缝，让上游错误包含合成密码/token，断言 `[REDACTED]` 或完全移除。当前用例只断言本地已知输出不包含夹具秘密。

8. **`status --check` 的最终语义**。
   合同把联网检查归到 `diagnose`。需要确认 Rust 是拒绝、移除还是保留为只读兼容参数；若保留，需传输 spy 证明它不联网、不写状态。

9. **Windows 用户访问边界**。
   Unix 用例只检查 `0600`；Windows 需要 ACL 等价验证（仅属主可读），放在 Windows runner 上，不能在 macOS 上假装通过。
