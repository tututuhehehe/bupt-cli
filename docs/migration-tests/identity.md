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

`src/identity.rs` 已实现本地 `login`、只读 `status`、会话归属判定和 `logout`；本轮新增 `src/identity/session.rs`，把 `src/identity/cas.rs` 的 CAS/UCloud 协议接到生产 `login` 和 `require_ucloud_session`，但仍只使用注入的 `Transport`。CLI 的传输工厂仍是 `Offline`，等待整合者接线。

- 有效的 `login --user … --password … --role …` 先预检本地目录和文件，再走 CAS；成功后写 `credentials.json` 和原生 `session.json`。生产入口不再返回 pending；交互式账号/密码提示仍待实现。
- 登录只提交一次密码表单，并且只以普通 CAS service-ticket 校验返回的显式重定向和 UCloud ticket 作为成功证据。登录本身不请求 UCloud token；ticket、最终 URL 和 Cookie 原文都不进入输出。
- 原生 session 用维护中的 `cookie_store` 序列化 Cookie，保留 host-only、默认 path、过期和非持久 Cookie；导入时拒绝 CAS 白名单之外的域名/path。`status` 同时兼容旧的平铺 Cookie 形状，公开输出字段不变。
- 当前 `login` 仍是非交互解析里程碑：缺少 `--user`/`--password` 会 usage 退出；最终登录仍必须恢复交互式提示。
- `status` 的 `user` 只输出 `user_id`、`user_name`、`real_name`；未知键忽略。`user_id` 只接受字符串或整数，两个名称字段只接受字符串；对象/数组等嵌套值直接省略。缓存中的 `mobile_jwgl.last_error` 不原样输出，统一映射为不含细节的安全诊断。
- 没有密码的已保存 profile 可以在 `status` 中显示学号，但不会激活任何缓存会话；没有 profile 时同样不激活。
- `locally_valid` 要求至少一个可用的 CAS Cookie：非空 domain、非空 name、字符串 value，且未明确过期。过期或格式不完整的 Cookie 只增加 `cookie_count`，不能单独让状态变成可用；缺失/不可解析的过期时间仍按未知处理。
- `diagnose` 已实现完整的本地编排：公共端点始终匿名检查，鉴权端点只使用已验证、归属匹配的原生 CAS 会话；没有可用会话时返回 skipped 行，不提交密码、不写会话。详见下文“诊断编排（本轮）”。
- `logout` 分别 unlink 自己拥有的 `credentials.json` 和 `session.json`，不重写打开的文件；`config.json` 和 `timetable.json` 保持原样。符号链接和非普通文件会被拒绝并报 auth 错误。
- 本 lane 的 28 个进程测试全部通过；`identity` 模块另有 58 个 lib 单测（含 5 个新增移动 JWGL 惰性 token 用例），全库 lib 共 302 个；待实现场景没有以永远通过或伪造断言的形式保留。

## 共享本地 profile API（本轮）

`src/identity.rs` 提供 crate-internal、只读的 `load_profile(config_dir)`，返回 `Option<LocalProfile>`：

- `LocalProfile::username() -> &str`：只读取 `credentials.json` 的 `username` 字符串，trim 后非空才返回。
- `LocalProfile::role() -> Option<Role>`：只用同一文件的 `role` 字符串做类型化解析；缺失和非法角色都是 `None`。
- `LocalProfile::has_password() -> bool`：只报告 `credentials.json` 中是否有非空密码字符串，不保存或返回密码内容。
- `require_authenticated_profile(config_dir)`：在 `load_profile` 之上要求 username 和非空密码；缺失、不可读或不可解析统一返回 auth 退出码 `3`，不检查 role。

`LocalProfile` 不包含 password/cookie/token 字段；`Debug` 只显示 username/role/has_password，新增的私有 `session_revision` 不显示也不通过公开访问器返回。这个 API 只读 `credentials.json`，不读取账号环境变量、会话或网络，也不写文件。

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

## CAS/UCloud 协议与原生会话（本轮）

`src/identity/cas.rs` 仍是单个平铺子模块，使用注入的 `Transport`；`src/identity/session.rs` 负责生产登录、磁盘加载和惰性 token 交换。`Cargo.toml` 只为 `cookie_store` 启用 `serde_json` feature，没有新增网络依赖。

- `cas_login` 先 GET CAS 登录页并提取 `execution`，然后只提交一次账号密码表单。所有重定向都校验 URL、host 和 path；POST 不跟随 307/308，返回明确的 auth 错误，避免重放密码。其余重定向只以 GET 跟随，目标必须是 HTTPS 的 `auth.bupt.edu.cn/authserver/*` 或 `ucloud.bupt.edu.cn/uclass*`；端口、userinfo、非 TLS、外部 host、路径穿越和超长重定向链都拒绝。TLS 证书校验由最终传输实现负责；本切片只保证请求 URL 和重定向目标不会离开 HTTPS 白名单。
- CAS 表单最终页必须是 2xx，且不能仍是 `/authserver/login` 登录页。随后必须再走普通 CAS service-ticket 校验请求：只有 301/302/303/307/308 这类明确重定向状态，且 `Location` 指向允许的 UCloud path 并携带 ticket，才算登录成功。2xx 即使带 `Location` 也不算；空页、无关页、登录页、300/304、缺失 ticket 或无 `Location` 的 3xx 都 fail closed。
- `CasLogin` 的字段私有，只有拿到 service ticket 才能构造；最终 URL 不保留，ticket 不进入 `Debug`。`session::login` 只在 `cas_login` 成功后读取旧 `credentials.json`、生成新 revision，并写入原生 session；验证失败不会触碰旧文件。
- Cookie 容器改为维护中的 `cookie_store`：接收时只接受 CAS 及其校园父域，导入时再次检查每个序列化 Cookie 的 domain/path，拒绝外部范围；序列化保留 host-only、默认 path、过期时间和非持久 Cookie。CAS Cookie 只发给 CAS，UCloud token/API 请求不带 Cookie。
- `ucloud_token` 是惰性的：CAS 登录本身不请求 OAuth token。协议层 `CasLogin` 在成功时保留一个一次性 ticket，供同一进程的首次 token 请求使用；生产 `session::login` 在写盘前丢弃它，所以独立进程首次读取会用持久化 Cookie 再走一次普通的 service-ticket 校验。之后 ticket 已消费或 token 过期时，同样重新校验并取新 ticket。token 缓存在 session 中，并按既有 `UCLOUD_EXPIRY_BUFFER_SECONDS`/JWT `exp` 判断是否过期；ticket 交换不跟随进入 UCloud。后续读取使用 `Blade-Auth`、`Tenant-Id: 000000` 和既有 UCloud 客户端 Basic 授权头，不把 CAS Cookie 或密码带到 UCloud API。
- `require_ucloud_session(context)` 返回 `(access_token, user_id)`；没有可验证的本地会话、Cookie 范围不合法、文件权限不安全或 revision 不匹配时，在联网前返回 auth `3`。有新鲜缓存 token 时直接复用且不写盘；缺失/过期时才做一次 service-ticket/token 交换，并把新 Cookie 和 token 写回 session。该路径从不提交账号密码。
- 错误按全局合同分层：401/403 是 auth `3`；其他 HTTP 4xx/5xx（包括限流 429）是 upstream `5`，其中 429 只提示等待，其他提示服务响应异常，均不提示重试密码；连接/传输失败是 network `4`。缺少 execution、畸形页面/JSON、缺失 `Location` 是 upstream `5`；没有 ticket、ticket 重定向不合法或缺少 `access_token` 是 auth `3`。错误消息不回显密码、Cookie、ticket、token 或最终 URL。
- 19 个 CAS/UCloud lib 单测使用 `Scripted` 和 `TEST-ONLY-` 合成值：单次密码 POST、service ticket 正向证据、显式重定向状态（301/302/303/307/308）、允许 302 后空/无关 200 无 ticket 的失败关闭、200 带 `Location` 不得当 ticket、307/308 拒绝、非 TLS/外部 host 拒绝、HTTP 4xx/5xx 与连接失败分层（含 429）、缺失 execution、惰性 token 与缓存复用、过期 token 不重用、UCloud 读取路径校验、ticket 重定向校验、token 响应成功条件。

**集成门槛（live transport）**：CLI 仍使用 `Offline`，没有真实校园流量。接线前仍需由整合者确认受限传输的 TLS/重定向行为、进程级 login/read 路径、`homework` 等业务命令的实际调用点，以及真实校园网验收。当前安全边界是固定 HTTPS host/path、单一密码 POST、Cookie 域/path 白名单、私有文件写入和非 Unix fail closed；不把合成测试当作真实登录证明。

## 生产身份/会话接线（本轮）

`session::login` 把“验证”和“登记”分开：

- `LoginRequest` 保存结构化的 `username`/`password`/`role`；`Debug` 只显示 username/role，password 固定为 `<redacted>`。
- 登录前预检：config/cache 目录不存在时创建为 `0700`，拒绝 symlink、非目录、组/其他用户可写；已有 `credentials.json`/`session.json` 拒绝 symlink、非普通文件或 owner 与父目录不一致；在目录中创建并删除私有 probe，确认能写入后才提交密码 POST。生成 32 字节随机 `session_revision`（hex），只用于本地归属绑定，不进入输出。
- CAS 成功后先合并旧 credentials 的未知键，再写 `username`/`password`/`role`/`session_revision`；session 重置协议相关状态，写入版本、账号、角色、revision、时间、维护中的 Cookie 数组、`cas_logins_total`，不写 ticket、最终 URL、密码或 token（登录阶段没有 token，协议层的一次性 ticket 也在写盘前丢弃）。
- 两个文件都在同目录以 `create_new` + `0600` 临时文件 staging；写字节前再次 `chmod 0600`，`sync_all` 后 `rename`，失败删除临时文件。代码不声称跨两个路径的原子事务：先提交 credentials，再提交 session；若 session 提交失败，credentials 的新 revision 会使旧 session 无法通过绑定检查。CAS/auth/network/限流失败或验证不完整时，旧 credentials/session 字节和权限保持不变。
- `require_ucloud_session` 读取时要求 credentials 和 session 都是普通、owner-only（组/其他用户无权限，通常为 `0600`）文件，父目录不是 symlink 且不可被其他用户写，config/cache 属主一致；校验 `version`、username、role、revision 后才接受 Cookie/token。旧 v1 会话仍由公开 `status` 只读兼容，但不会激活原生读取路径。
- 非 Unix 平台在写入前 fail closed（upstream `5`），不声明 Windows ACL 支持。
- 17 个新增 lib 单测覆盖：单次 CAS 验证且登录不请求 token、credentials 未知键保留和新 revision、second login 生成新 revision、分离进程从磁盘加载并复用新鲜 token（含 CAS Cookie 已过期）、无密码/错误 revision/外部 Cookie 范围/不安全权限在联网前拒绝、过期 token 惰性刷新并写回、token 刷新 401 时 session 不变、401 和缺少 ticket 时旧字节不变且不重试密码、损坏的普通 credentials/session 在验证成功后修复、symlink/可写目录在密码 POST 前拒绝、非 Unix fail closed。测试只用临时目录、`Scripted` 和合成秘密。

## 移动 JWGL 惰性 token（本轮）

公开给同 crate 其他模块的接口：

```rust
pub(crate) fn identity::require_mobile_jwgl_token(
    context: &mut Context<'_>,
) -> CliResult<String>
```

- 只允许本科账号。先读取私有的 `credentials.json`，role 不是 `undergraduate`（含缺失/非法 role）在读取会话和发出任何请求前返回 auth `3`；研究生不会提交密码，也不会复用旧的本科 token。
- 通过原生 session 的 owner-only 文件、`version`、`username`、`role` 和 `session_revision` 绑定校验后，先尝试复用 `mobile_jwgl.token`：显式 `expires_at` 仍有效，或缺失时从 JWT `exp`（移动端可能是毫秒）推导出有效时间，则直接返回，不读取 UCloud/CAS 状态、不写文件、不联网。缺少 expiry 且不是 JWT 时按保守的 30 分钟本地有效期复用。
- token 缺失或过期时，先验证 cache 目录可安全写入，再从 `credentials.json` 读取密码。整个命令只向固定 URL `https://jwglweixin.bupt.edu.cn/bjyddx/login` 发一次 `POST`，表单字段固定为 `userNo`、`pwd`；不跟随重定向、不重试、不提交 CAS 表单、不使用浏览器。传输仍由注入的 `Transport` 提供。
- 登录响应必须是 JSON 对象且 `code` 为数字/字符串 `1`，`data.token` 为非空、无空白/控制字符的字符串；移动端 JWT 的 `exp` 若是毫秒会转换为秒，再减去 120 秒刷新缓冲；无 `exp` 的 token 使用 30 分钟保守有效期。
- 成功时只改原生 session 的 `mobile_jwgl.token`、`mobile_jwgl.expires_at` 和 `updated_at`：其他 `mobile_jwgl` 字段、CAS Cookie、UCloud token、`cas_logins_total`、创建时间及未知顶层字段全部保留。写入沿用私有 `0600` 临时文件 + `rename` 的分阶段路径，失败会删除临时文件并保留旧 session 字节；返回 token 前必须已完成持久化。
- 失败时旧 `credentials.json`/`session.json` 字节和权限不变，且不会重新提交密码。401/403、`code` 拒绝、缺 token 是 auth `3`；连接/传输失败是 network `4`；其他 HTTP（含 429、3xx）或畸形 JSON/缺 `code` 是 upstream `5`。错误不包含密码、token、响应正文或请求 URL。
- **兼容性限制（明确）**：移动 JWGL 校验的是 `jwgl.bupt.edu.cn` 的 JWGL 密码，可能与统一身份认证/CAS 密码不同。本迁移只有 `credentials.json` 中的一个密码，不提供第二个 JWGL 密码字段、环境变量或公开参数；如果北邮拒绝该密码，`require_mobile_jwgl_token` 返回 auth 错误并明确提示这一差异，**不会**重试或尝试另一密码。只有 CAS/WJGL 密码相同的本科账号可以自动获取；不同密码的账号本轮无法获得移动 token。
- `diagnose` 保持只读、无密码：移动行仍只使用缓存 token，不调用本 helper，也不提交 JWGL 登录。

新增 5 个 `Scripted` lib 用例：研究生角色在传输前拒绝；owner/revision 绑定且未过期的 token 在 CAS/UCloud 半损坏时仍复用且零写入；过期 token 只发一次固定 POST 并只更新 mobile 范围、保留其他 session 字段；302/401/403/429/500、非 JSON、缺 code、拒绝 envelope、缺 token 以及网络失败都只发一次请求并保留旧字节；错误 revision 在密码 POST 前拒绝。

## 诊断编排（本轮）

`src/identity/probe.rs` 把 Python `probe.py` 的端点矩阵搬到 Rust，但所有网络请求都走注入的 `Transport`，没有任何测试专用分支或真实浏览器：

- `diagnose` 不再返回 pending。`run_diagnose` 的 stub 和旧的 `skipped_cas_report` 已删除；`identity::run` 直接调用 `probe::run(matches, context)`。
- 公共可达性检查和鉴权检查分离。公共行只发固定的匿名 GET；需要会话的行先经过 `load_owned_cas_session(context)`，要求 owner-only 文件、原生 v2 会话、`(username, role)` 和 `session_revision` 全部匹配，且 Cookie 可用。没有可用会话或会话文件不安全时，只增加 skipped 行，不触碰传输。
- 诊断**从不**提交登录表单、密码、登录用户名或密码头；CAS 行只请求普通 service ticket，UCloud 行只把 ticket 交换成内存 token。刷新得到的 Cookie/token 只留在 `ProbeRun` 内，不调用 `write_session_json`，所以失败或成功都不改变 `credentials.json`/`session.json` 字节。已有新鲜、绑定账号的 UCloud token 直接复用，不再花费一次 CAS ticket；只有 token 缺失或过期才走惰性交换。
- 没有任意目标参数：ticket 只能由 `CasService` 的封闭枚举取得，目前仅有 `UCloud`。`OwnedCasSession::ticket_for` 只发出预声明的 CAS service URL；`cookie_header_for` 只对固定 portal/electricity 路径生成 Cookie 头。UCloud token/API 请求不带 CAS Cookie。
- 错误分层写入行：传输 auth `3` -> `auth:`；连接/传输 network `4` -> `unreachable:`；其他上游/HTTP 4xx/5xx（含 429）-> `upstream:`。行里不输出异常原文、Cookie、ticket、token 或完整重定向 URL。
- 运行顺序与 Python 一致：公共端点 -> CAS ticket -> UCloud token（惰性）-> portal -> electricity -> UCloud API -> mobile JWGL；报告按组和名称稳定排序，`auth`/`summary`/`session_path` 字段与 Python 对齐。

### 固定协议端点（供父 lane 接线）

| 用途 | 方法和 URL |
| --- | --- |
| CAS 登录页（公共） | `GET https://auth.bupt.edu.cn/authserver/login` |
| CAS service ticket | `GET https://auth.bupt.edu.cn/authserver/login?service=https%3A%2F%2Fucloud.bupt.edu.cn%2Fuclass` |
| ticket 允许的最终目标 | `https://ucloud.bupt.edu.cn/uclass`（只接受明确的 301/302/303/307/308 和有效 ticket） |
| UCloud token | `POST https://apiucloud.bupt.edu.cn/ykt-basics/oauth/token` |
| UCloud API 基址 | `https://apiucloud.bupt.edu.cn` |
| UCloud Basic | `Basic c3dvcmQ6c3dvcmRfc2VjcmV0` |

公共检查还使用固定 URL：`https://uc.bupt.edu.cn/api/uc/status`、`https://win.bupt.edu.cn/note.do`、`https://jwgl.bupt.edu.cn/jsxsd/framework/xsMain_bjyddx.jsp`、`https://jwgl.bupt.edu.cn/jsxsd/framework/xsdPerson.jsp`、`https://jwxt.bupt.edu.cn/caslogin.jsp`、`https://jwglweixin.bupt.edu.cn/bjyddx/login`、`http://my.bupt.edu.cn/list.jsp?...`、`https://app.bupt.edu.cn/buptdf/wap/default/chong`。这些只是诊断探测，不是业务 API 白名单；父 lane 仍负责最终传输策略和命令接线。

### 新增诊断测试

`src/identity/probe.rs` 内 9 个 lib 测试全部使用 `Scripted` 和 `TEST-ONLY-` 合成值：公共组只发 3 个固定 GET 且不建文件；无会话时鉴权行 skipped 且零调用；CAS ticket 行只发一次 GET、带 owned Cookie、无登录 POST、凭据/会话字节不变；auth/network/upstream 三类传输错误及 HTTP 401/403/429/500 分别落为 `auth:`/`unreachable:`/`upstream:`；`--only ucloud` 完成一次惰性 ticket+token 交换、9 个 UCloud 行不带 Cookie、刷新后会话字节不变；无 Cookie 的新鲜 token 直接复用且不访问 CAS/token 端点；portal 和 electricity 只使用匹配路径的 Cookie，不发送 CAS Cookie/密码；本科生缓存 mobile token 时只走 mobile 端点，凭据和会话保持不变。

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
- Python `login` 在验证前保存新凭据；Rust 先验证 CAS，再在同一事务顺序中提交新 credentials 和 session。失败时保留上一次有效登记和会话；若 session 提交单独失败，新 revision 会让旧 session 失效，而不是假装两者已原子提交。
- Python `logout` 只删 `session.json`，不清密码；合同要求同时忘掉本地凭据和会话，但保留课表和设置。Rust 直接 unlink 两个自有文件，不重写或截断；符号链接/非普通文件拒绝。
- Python `status.user` 原样返回缓存对象；Rust 只允许 `user_id`、`user_name`、`real_name`，且只接受字符串/整数 ID 和字符串名称，嵌套值不输出。Python `mobile_jwgl.last_error` 原样返回；Rust 映射为安全诊断，不输出缓存自由文本。
- Python 的凭据读取允许环境变量覆盖 `credentials.json`；Rust 合同改为只读 JSON，账号环境变量不再补充或覆盖任何字段。相关进程用例验证 env-only 被忽略、冲突 env 不改变本地账号和 role。
- Python 只要缓存有 Cookie 且学号相同就可能复用；Rust 要求 profile 含密码、v2 归属元组 `(username, role)` 和 revision；公开 `status` 仍要求至少一个未明确过期且 name/value 完整的 CAS Cookie，原生读取在已有新鲜 token 时可直接复用 token，只有需要刷新 token 时才要求可用 CAS Cookie。v1 只复用身份无关的 CAS/UCloud 状态，不复用无法确认角色的移动教务状态。
- Python 的 `session.json` 使用平铺 Cookie 对象；Rust 原生登录使用 `cookie_store` 序列化，同时让公开 `status` 继续读取旧平铺形状。Python 的旧会话没有 per-login revision；Rust 原生会话要求 revision，防止同账号改密后旧 session 继续被读取。
- 当前 Rust `login` 只实现非交互参数解析；Python 在缺参数时提示输入。最终登录仍必须恢复交互式提示；本轮只把它记录为里程碑，不把非交互行为当作最终合同。
- 成功/错误输出统一带 `schema: "bupt.v1"`；这是迁移合同的新增外壳。

## 待实现场景与所需测试缝

以下场景仍不能在当前进程级共享 harness 下完整验证。生产登录/读取已通过 `Scripted` 接上注入传输，但 CLI 仍是 `Offline`，进程级验证、真实传输和平台 ACL 仍待补；它们需要整合者先确认最小测试缝，不能用永远通过的空断言或猜测内部 API 代替。

1. **延迟 CAS 拒绝时保留旧状态**（与非法参数用例分开）。
   `session` 的 `Scripted` 测试已覆盖参数合法后 CAS 401、缺少 service ticket 时旧 `credentials.json`、旧 `session.json` 字节和权限不变，且只有一个密码 POST。CLI 仍用 `Offline`，所以进程级延迟拒绝仍要在真实传输接线后复跑；当前 `invalid_login_arguments_...` 只覆盖联网前拒绝，不能代替进程级场景。

2. **同一学号改密码后旧会话失效**。
   原生 session 现在绑定 username、role 和每次登录新生成的 `session_revision`；成功登录会同时写新 revision 的 credentials/session，单独更新 credentials 的异常路径也会让旧 session 被拒绝。离线测试已证明新密码走完整 CAS、旧密码/旧 token 不出现在请求中；仍缺真实密码修改的进程级场景和旧 session 文件的显式清理策略。

3. **成功登录的凭据/会话存储与续期写回**。
   已覆盖两个文件的私有创建/替换、`0600`、未知键合并保留、revision、无 ticket/URL 持久化、非私有目录/文件和 symlink 拒绝，以及成功登录不请求 token。仍需真实传输接线；macOS `0600` 之外尤其 Windows 用户 ACL 未实现，非 Unix 写入 fail closed。当前进程用例仍只验证已存在文件的读取/删除边界，不验证生产创建。

4. **续期有界且只由明确过期触发**。
   需要脚本化传输序列和请求/登录计数观测：缓存票据失效时最多一次自动续期；达到 `BUPT_MAX_CAS_LOGINS` 预算后不再登录；暂时断网、密码拒绝、HTTP 401/403、限流（429）不得被当作过期信号触发续期。当前只实现按需 service-ticket/token 刷新，没有自动 CAS 密码重登或登录预算；仍只能验证 `status` 不续期、不改变登录计数、缺失过期时间不触发重登。

5. **`diagnose` 的进程级在线路径**。
   lib 级 `Scripted` 用例已覆盖公共/移动教务无 token 路径、请求体不含密码、UCloud ticket/token 刷新不写会话、移动教务只用缓存 token；仍缺父 lane 把受限传输接进 CLI 后的进程级复跑，包括真实 transport 的超时、TLS 和重定向策略。当前进程级只验证无会话跳过和未知组 usage，不能在离线共享 harness 里意外联网。

6. **上游回声中的秘密脱敏**。
   诊断行只输出稳定的错误分类，不复用传输错误原文；未来若加入上游消息字段，仍需用可注入错误消息的故障缝验证 `[REDACTED]` 或完全移除。

7. **Windows 用户访问边界**。
   Unix 用例只检查 `0600`；Windows 需要 ACL 等价验证（仅属主可读），放在 Windows runner 上，不能在 macOS 上假装通过。

8. **交互式登录提示**。
   最终 `login` 必须恢复缺参数时的交互式账号/密码提示；当前非交互参数解析只是本地里程碑，不能作为最终合同。
