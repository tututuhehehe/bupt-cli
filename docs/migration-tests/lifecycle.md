# lifecycle 测试清单

范围：`help` / `--help` / `--version`、机器输出与错误信封、`update` / `uninstall` 的离线安全边界。权威行为以 [Rust 迁移合同](../rust-migration.md) 为准；Python 快照 `d7c35ef` 只作对照。

## 文件

- 用例：`tests/contract_lifecycle.rs`（共享 `mod contract_support;`）。
  - 所有卸载子进程用例（含 `--dry-run`、`--keep-data` 和真实删除）都把 CLI 二进制复制到临时目录再运行，环境仍取自 `Sandbox::command()`；这样“当前正在运行的 bupt”不会指向 `target/` 里的测试构建产物，也绝不触碰真实已安装程序。
  - 文件快照用 `symlink_metadata` 遍历，I/O 错误直接失败，不跟随 symlink；混合目录/危险根/符号链接目标都按字节比较。
- 夹具：`tests/fixtures/contracts/lifecycle/data/`。全部为合成数据，不含真实账号、Cookie、token 或密码。
  - `credentials.fixture.json` / `session.fixture.json`：只用于验证卸载会清理“自己的文件”。
  - `config.fixture.json` / `timetable.fixture.json`：自己的配置与课表缓存；课表带可识别的 `version` / `courses` / `calendar` 载荷。
  - 文件名带 `.fixture` 是刻意的：避免与真实状态文件名混淆，也避免被仓库的凭据忽略规则吞掉。
  - `unrelated.txt`：卸载时不得删除的邻居文件；同名 `timetable.json` lookalike 在测试里用临时文件构造。
- 本清单不写入任何真实秘密；测试失败诊断只输出 JSON 和路径。

## 已覆盖

| 合同点 | Python 对照 | Rust 用例 |
| --- | --- | --- |
| `help` / `--help` 保持文本，且列出完整公开命令树 | `test_help_subcommand_prints_usage`、`test_help_covers_every_registered_command` | `help_overview_is_text_and_lists_the_public_tree` |
| `help <命令>` 覆盖每个公开命令 | 同上 | `help_topics_cover_every_public_command` |
| 两级命令树：作业、课表、图书馆、电费楼栋、通知详情 | `cli.build_parser` | `help_topics_list_the_canonical_nested_commands` |
| 删除 `homework list`、`schedule exams`、`probe` 旧名，连 `--help` 也不放行 | `test_help_subcommand_prints_usage` 中的旧 `homework list` 预期；合同的有意变化 | `removed_command_names_are_not_aliases` |
| 未知 help 主题是 usage JSON，不是文本/裸 argparse | `test_help_subcommand_rejects_unknown_topic` | `unknown_help_topics_are_usage_errors_on_stderr` |
| `--version` 保持文本、不产生本地状态 | `build.sh` 冒烟测试；`cli.py` 的 `--version` | `version_is_text_and_does_not_touch_state` |
| 无命令是 usage 错误（退出 2） | `test_usage_error_for_missing_command` | `missing_command_is_a_usage_error` |
| 成功结果只写一个带 `schema: "bupt.v1"` 的 stdout JSON 文档 | `test_success_envelope_has_ok`；合同新增 schema | `status_is_one_compact_json_document_with_schema` |
| `--pretty` 只改排版，不改值；全局与子命令之后均可 | `test_pretty_flag_after_subcommand`；合同明确“`--pretty` 不改变值” | `pretty_changes_layout_without_changing_values`、`pretty_changes_error_layout_without_changing_values` |
| 错误只写一个 stderr JSON，stdout 为空；usage=2、auth=3 | `test_error_envelope_exit_codes` | 各错误用例统一经 `assert_error_json`；`homework_uses_the_canonical_default_entry` 覆盖 auth=3 |
| 非法参数在读写业务状态前拒绝；不设 `--json` | `test_unknown_option_is_json_usage_error`、`test_invalid_numeric_arguments_are_usage_errors` | `invalid_arguments_are_usage_errors_before_local_state_changes` |
| `homework` 只用默认入口；`homework list` 已删除；过滤器归 homework lane | `homework list` 旧入口；合同改为默认入口 | `homework_uses_the_canonical_default_entry`、`removed_command_names_are_not_aliases` |
| `uninstall --dry-run` 不改文件，并报告将清理的自己的数据 | `cmd_uninstall` | `uninstall_dry_run_lists_owned_data_without_changing_anything` |
| `uninstall --keep-data` 保留凭据、会话、课表，并标为 skipped | `cmd_uninstall`、`test_uninstall_refuses_home_and_root` | `uninstall_keep_data_preserves_credentials_and_timetable` |
| 未知文件与自己的文件混在一起时整目录跳过，双方文件都按字节保留并报告 skipped | `test_uninstall_refuses_home_and_root` 的保守保护；合同“不扩大删除范围” | `uninstall_skips_mixed_data_directories_wholesale` |
| 只有自己可识别的 `timetable.json` 时也能完整清理 | 旧 `data_markers` 不含 `timetable.json`，这是要修的缺口 | `uninstall_removes_an_owned_timetable_only_directory` |
| 不因 basename 是 `timetable.json` 就删除用户目录里的同名文件 | 旧 `looks_like_bupt` 的“先验证再删除”原则 | `uninstall_does_not_remove_a_timetable_lookalike` |
| `timetable.json` 归属识别要求 v1 的 term/role/shape，v2 另需 username | 旧 `data_markers` 缺口；合同“只清理自己的课表” | `timetable_ownership_requires_known_shape`、`owned_data_requires_recognized_payloads` |
| 卸载 note 区分完整完成、keep-data 保留、跳过或部分失败 | `cmd_uninstall` 的 `note`；合同“不对跳过项报告为已删除” | `uninstall_note_distinguishes_completion_from_skips_and_keep_data`；`uninstall_skips_mixed_data_directories_wholesale`、`uninstall_keep_data_preserves_credentials_and_timetable` 的 note 断言 |
| 危险数据根在真实删除模式下也不动文件 | `test_uninstall_refuses_home_and_root` | `uninstall_refuses_dangerous_data_roots_in_destructive_mode` |
| 数据目录是符号链接时不跟随目标，只处理链接本身或跳过 | `cmd_uninstall` 的 symlink 分支 | `uninstall_does_not_follow_a_symlinked_data_directory` |
| 不盲删显式 PATH/CWD 上恰好叫 `bupt` 的无关可执行文件 | `test_looks_like_bupt_rejects_random_binary`、`test_uninstall_never_targets_source_files` | `uninstall_does_not_delete_an_unrelated_executable_on_path` |
| 不把源码文件当作可卸载二进制（Unix 可执行位） | `test_uninstall_never_targets_source_files` | `uninstall_does_not_delete_a_source_file_named_like_the_binary` |
| `update` 的帮助保留 `--check` / `--repo OWNER/REPO` | `cli.build_parser` | `update_help_documents_check_and_repo_inputs` |
| `update` 的仓库输入在联网前校验；非法参数不落盘 | 合同“非法参数在…联网前拒绝”；`--repo` 的 `OWNER/REPO` 形态 | `update_rejects_invalid_inputs_before_release_access` |
| `update --check` 用 GET `/releases/latest` 且不跟随重定向；字段和退出信封沿用 Python | `_release_tag`、`test_update_uses_release_redirect_not_the_api`、`test_release_tag_reads_the_redirect` | `update_check_reports_a_newer_release_without_side_effects` |
| 无 release 是成功结果：`latest=null`、`update_available=false`、有说明 | `test_update_reports_missing_release` | `update_check_reports_missing_release_and_no_upgrade` |
| 只接受同一仓库 `releases/tag/<tag>` 的 HTTPS Location；外来主机、其他仓库、畸形重定向失败关闭 | 合同“校验重定向目标不扩大访问范围” | `release_location_must_name_the_same_repository_tag`、`update_check_rejects_a_foreign_redirect` |
| 版本按数字段比较，旧 tag/等版本不算升级 | Python `_versions_tuple` | `version_ordering_does_not_promote_older_tags`、`update_check_reports_missing_release_and_no_upgrade` |
| release 检查区分传输失败=4 network、HTTP=5 upstream、401/403=3 auth，且零文件副作用 | 合同错误分层；`from_campus_error` | `release_check_layers_transport_and_http_failures`、`update_check_reports_a_newer_release_without_side_effects` |
| 生产 `Offline` transport 下运行时 `update --check` 明确为 network=4，不写状态 | 合同“runtime check blocked on scoped production transport” | `update_check_is_blocked_without_a_scoped_production_transport` |
| 默认 `update`（无 `--check`）仍 pending，不联网、不写文件 | 合同“apply remains pending” | `apply_update_remains_pending_without_network_or_files` |

## 实现（离线与本地路径）

实现文件：`src/lifecycle.rs`。只使用临时目录、合成 JSON 和假发行物名；没有联网，没有扫描或删除真实安装，也没有执行真实的 `update`。

### `update`

- `--check` / `--repo OWNER/REPO` 保留；`--repo` 只接受严格的 `OWNER/REPO`，URL、空值、额外路径段在联网和写状态前以 usage（退出 2）拒绝。
- `--check` 通过注入的 `Transport` 发起一次 `GET https://github.com/OWNER/REPO/releases/latest`，带 `User-Agent: bupt-cli`；不跟随重定向，不访问 `api.github.com`，不下载 asset。
- 只有 301/302/303/307/308 且 `Location` 是同一仓库的 `https://github.com/OWNER/REPO/releases/tag/<tag>` 才接受；主机、仓库或路径不符、query/fragment、额外路径段、缺失 Location 都以 upstream（退出 5）失败关闭。`<tag> == latest` 保留 Python 的“无版本号”成功结果。
- 成功字段沿用 Python：`current`、`repo`、`latest`（去掉前导 `v`）、`update_available`、`note`。`current` 取编译时的 `CARGO_PKG_VERSION`（当前迁移包为 `0.0.0`），不硬编码 Python 的 `__version__`。无 release（404 且无有效 Location）是成功结果（`update_available=false`），不是错误。
- 版本比较只取前三个数字段，并按 Python 的短元组规则比较，`1.10.0 > 1.9.0`；相等或更旧的 tag 不报告升级。
- 错误分层：传输失败=4 network；401/403=3 auth；其他 HTTP 状态和畸形/外来重定向=5 upstream。检查不写 config/cache/暂存文件。
- 发行物按平台选择：macOS Apple Silicon 用 `bupt-macos-arm64.zip`，Windows x64 用 `bupt-windows-x64.zip`。Windows 名称沿用 macOS 的命名习惯，发布工作流尚未确认；这是需要整合者确认的歧义点，目前只用单元测试固定选择逻辑。
- 更新目标只从当前正在运行的、已验证的可执行文件取得，不依赖 Python 的 `sys.frozen` 语义。
- 下载、完整性校验、备份替换、Windows 运行中 `.exe` 替换和实机 runtime check 仍待实现；生产 `Offline` transport 下运行时 `update --check` 会在联网前返回 network（退出 4）。默认 `update`（无 `--check`）保持原有 pending 行为，不联网、不写文件。

### `uninstall`

- 可执行文件候选只有当前正在运行的 `bupt` / `bupt.exe`：必须是普通文件、非空、文件名匹配且（Unix）有执行位；验证失败时只记为 skipped。不扫描 PATH，不删除其他同名条目，不把源码文件当作二进制。
- 数据目录先完整规划并携带已验证的普通文件集合：遇到未知 basename 先短路，不读取其内容；符号链接、危险根（根目录、HOME 或其上级、当前目录或其上级）、`--keep-data`、未知/混合/不可验证内容，全部记为 skipped，不触碰任何条目。
- 只有目录内所有文件都可识别为自己的 JSON（`credentials.json`、`session.json`、`config.json`、`timetable.json`，且内容形态匹配）时才执行删除。`timetable.json` 要求 `version` 1/2、`term`、已知 `role`、`courses` 数组和 `calendar` 对象；v2 还要求 `username`。v1 的已验证夹具继续接受，同名但内容不符的 lookalike 会跳过。
- 删除按计划中的叶子文件逐个进行，最后只调用非递归 `remove_dir`。开始删除前会再次核对目录身份、条目集合和文件元数据；若发现新条目、替换的路径或变化的文件元数据，整个目录跳过。删除开始后若再发现新条目或变化，会停止并保留剩余内容，报告已删除的文件和未完成的目录，不声称原子回滚。
- `--dry-run` 只生成计划，不删除可执行文件或数据；`--keep-data` 保留数据目录但移除当前可执行文件（Unix）。Windows 运行中 `.exe` 的移除/替换仍待实现，明确报告为 skipped，不报告为 removed。
- `note` 仍是单个字符串，不新增字段：`卸载完成` 只在没有 skipped、没有 keep-data 保留时出现；保留、跳过、删除失败或待处理会分别得到明确的部分完成/未移除说明。

### 本轮测试结果

```sh
cargo test --locked --offline --lib
# 82 passed（lifecycle::tests 23 个）

cargo test --locked --offline --test contract_lifecycle
# 24 passed; 0 failed
```

新增/相关的单元用例：`repo_must_be_owner_slash_repo`、`release_assets_are_selected_per_supported_platform`、`only_a_regular_bupt_executable_is_a_target`、`removal_plans_only_the_given_executable`、`dry_run_plans_without_removing_the_executable`、`owned_data_requires_recognized_payloads`、`timetable_ownership_requires_known_shape`、`logged_out_credentials_remain_recognizable`、`dry_run_keeps_the_vetted_directory`、`mixed_directory_is_rejected_before_removal`、`late_unknown_file_survives_execution`、`changed_owned_file_survives_execution`、`replaced_directory_survives_execution`、`uninstall_note_distinguishes_completion_from_skips_and_keep_data`、`dangerous_roots_are_refused_without_following_aliases`、`update_check_reports_a_newer_release_without_side_effects`、`update_check_reports_missing_release_and_no_upgrade`、`update_check_reports_a_redirect_without_a_version`、`release_location_must_name_the_same_repository_tag`、`update_check_rejects_a_foreign_redirect`、`release_check_layers_transport_and_http_failures`、`version_ordering_does_not_promote_older_tags`、`apply_update_remains_pending_without_network_or_files`。

此前因 identity / homework 尚未接入而失败的三条 lane 用例（`status_is_one_compact_json_document_with_schema`、`pretty_changes_layout_without_changing_values`、`homework_uses_the_canonical_default_entry`）在当前基线已通过；本轮 lane 合同测试全绿。

## 暂不能离线验证（需要已约定的测试缝）

以下场景仍未完成或需要整合者先确定最小测试缝，不能用假断言或猜测内部 API 代替。网络、真实 GitHub、真实已安装二进制和真实更新动作均不进入本轮测试。

1. **release 检查：离线已覆盖，真实 GitHub 未验收**
   - Python：`_release_tag`、`test_update_uses_release_redirect_not_the_api`、`test_release_tag_reads_the_redirect`、`test_update_reports_missing_release`。
   - 已覆盖：注入 `Transport`/`Scripted` 下的 GET、不跟随重定向、同仓库 tag 路径校验、无 release、畸形/外来重定向、版本边界、network/upstream/auth 分层和零文件副作用。
   - 仍缺：scoped production transport 和真实 GitHub 的 runtime check。当前生产入口仍用 `Offline`，运行时 `update --check` 直接返回 network（退出 4），不得把离线用例当作线上验收。

2. **更新 staging / 原子替换 / 失败回滚（暂缓）**
   - Python：`cmd_update`、`test_update_replaces_a_frozen_binary`、`test_update_install_path_from_source_is_honest`；`build.sh` 的“先打包到 staging、冒烟、再原子替换，失败不覆盖旧二进制”。
   - 缺口：需要可注入的 asset 字节和安装目标路径；目标必须是临时夹具，不能碰真实 `bupt`。native Rust 二进制的安装形态由实现显式判定，不把 Python 的 `sys.frozen` 语义当作公共前提。
   - 待验证：
     - macOS Apple Silicon 与 Windows x64 选择各自 asset，不把 macOS 包装到 Windows（或反之）。
     - zip 缺 `bupt`、截断/损坏 zip、写入失败、替换后大小不一致时，命令失败且旧目标仍可执行。
     - 成功时保留备份、清除 `.new` 临时文件，并确认替换结果；失败时不得报告“已更新”。
     - 未获准目标/非本版安装不得自动替换，只报告不安装或给出手动处理说明。
     - 迁移版不静默覆盖旧 Python 版：目标若是旧 Python launcher 或非本版安装，只能拒绝/提示，不能自动替换。

3. **`update` 的网络/上游错误分层**
   - Python：`from_campus_error`、`test_http_status_network_error_maps_upstream`、`test_http_401_network_error_maps_auth`（同属错误合同）。
   - release 检查已用可注入传输覆盖：传输失败=4 network，其他 HTTP 4xx/5xx=5 upstream，401/403=3 auth；错误路径只产生一个 stderr JSON、stdout 为空，成功路径只写 stdout JSON。
   - 仍缺：真实校外网络下的超时/断网和 scoped production transport 验收；不能用真实网络假装通过。

4. **凭据脱敏覆盖**
   - Python：`test_cli_error_messages_are_redacted`。
   - 缺口：需要 identity lane 提供的凭据注入/错误回显测试缝；不能在测试里使用真实密码。
   - 待验证：合成密码出现在上游错误文本里时被替换为 `[REDACTED]`；日志、help 和错误信封不携带秘密。

5. **卸载的 PATH/CWD 扫描范围**
   - 当前实现明确不扫描 `PATH`/CWD；只把当前正在运行的已验证可执行文件作为候选。用例把无关可执行文件/源码放在显式临时 `PATH` 与 CWD，并断言它不会被删除。
   - 若后续改为扫描 PATH/CWD，必须把被拒绝的候选放进 `skipped`，并重新审视“只作用于获准目标”的合同。

6. **在线更新与 Windows swap（暂缓、待实测）**
   - `update --check` 的协议逻辑已离线覆盖，但生产 `Offline` transport 尚未接入 scoped production transport：运行时 check 仍被阻断为 network（退出 4），真实 GitHub 检查未验收。
   - asset 下载、在线更新和 Windows 原位替换/运行中文件锁行为均未在本轮运行；不得用 live network/publish 假装通过。Windows 上的运行中 `.exe` 移除/替换仍只报告 skipped；下载和远端替换仍未实现、未声称完成。
   - macOS Apple Silicon 与 Windows x64 的真实 release 可执行文件、权限位、替换行为和 `--version` 冒烟测试需要两个平台的实际运行；Windows 替换和 runtime check 都阻塞在 scoped production transport 上。

## 历史红测记录

测试迁移阶段的 23 个用例曾在脚手架 `src/main.rs` 上全部失败，失败统一为：

```text
unexpected exit code; stdout=""; stderr="Rust CLI not implemented; test migration scaffold only\n"
left: 78
```

现在 lifecycle 的本地实现已接入，上述失败只剩其他功能线的待接用例（见“本轮测试结果”）。

## 格式与编译

- `cargo test --locked --offline --test contract_lifecycle --no-run`：通过。
- `cargo fmt --check` 通过（基线已包含共享 harness 的格式修复）。
- 共享 `tests/contract_support/mod.rs`、`Cargo.toml`、`Cargo.lock`、`src/cli.rs`、`src/transport.rs` 均未修改。
