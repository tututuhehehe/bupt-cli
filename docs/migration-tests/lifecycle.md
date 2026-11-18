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
| 危险数据根在真实删除模式下也不动文件 | `test_uninstall_refuses_home_and_root` | `uninstall_refuses_dangerous_data_roots_in_destructive_mode` |
| 数据目录是符号链接时不跟随目标，只处理链接本身或跳过 | `cmd_uninstall` 的 symlink 分支 | `uninstall_does_not_follow_a_symlinked_data_directory` |
| 不盲删显式 PATH/CWD 上恰好叫 `bupt` 的无关可执行文件 | `test_looks_like_bupt_rejects_random_binary`、`test_uninstall_never_targets_source_files` | `uninstall_does_not_delete_an_unrelated_executable_on_path` |
| 不把源码文件当作可卸载二进制（Unix 可执行位） | `test_uninstall_never_targets_source_files` | `uninstall_does_not_delete_a_source_file_named_like_the_binary` |
| `update` 的帮助保留 `--check` / `--repo OWNER/REPO` | `cli.build_parser` | `update_help_documents_check_and_repo_inputs` |
| `update` 的仓库输入在联网前校验；非法参数不落盘 | 合同“非法参数在…联网前拒绝”；`--repo` 的 `OWNER/REPO` 形态 | `update_rejects_invalid_inputs_before_release_access` |

## 暂不能离线验证（需要已约定的测试缝）

以下场景需要整合者先确定最小测试缝，不能用假断言或猜测内部 API 代替。网络、真实 GitHub、真实已安装二进制和真实更新动作均不进入本轮测试。

1. **release 检查**
   - Python：`_release_tag`、`test_update_uses_release_redirect_not_the_api`、`test_release_tag_reads_the_redirect`、`test_update_reports_missing_release`。
   - 缺口：需要可注入的 release 元数据/重定向解析器，或只读的本地 release 夹具源；不得访问 `github.com` / `api.github.com`。
   - 待验证：从 `/releases/latest` 形态读取 tag；无 release 时 `latest=null`、`update_available=false`、有说明；检查失败时是 network 错误且不破坏本地状态；不依赖 API token。

2. **更新 staging / 原子替换 / 失败回滚**
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
   - 缺口：可注入的传输层错误或本地假响应；不能用真实校外网络。
   - 待验证：超时/断网=4 network，HTTP 4xx/5xx 上游=5 upstream，401/403=auth=3；无论哪层都只产生一个 stderr JSON，stdout 为空。

4. **凭据脱敏覆盖**
   - Python：`test_cli_error_messages_are_redacted`。
   - 缺口：需要 identity lane 提供的凭据注入/错误回显测试缝；不能在测试里使用真实密码。
   - 待验证：合成密码出现在上游错误文本里时被替换为 `[REDACTED]`；日志、help 和错误信封不携带秘密。

5. **卸载的 PATH/CWD 扫描范围**
   - 当前用例把无关可执行文件/源码放在显式临时 `PATH` 与 CWD，并断言它不会被删除。
   - 如果最终实现选择扫描 PATH/CWD，它必须把被拒绝的候选放进 `skipped`；如果确定不扫描，则此用例只证明“不会误删”，扫描范围需在实现合同里写清。

6. **在线更新与 Windows swap（待实测）**
   - 真实 GitHub release 检查、asset 下载、在线更新和 Windows 原位替换/运行中文件锁行为均未在本轮运行；不得用 live network/publish 假装通过。
   - macOS Apple Silicon 与 Windows x64 的真实 release 可执行文件、权限位、替换行为和 `--version` 冒烟测试需要两个平台的实际运行。

## 红测记录

```sh
cargo test --offline --test contract_lifecycle -- --test-threads=1
```

结果：`0 passed; 23 failed`。全部失败都停在共享夹具启动的二进制：

```text
unexpected exit code; stdout=""; stderr="Rust CLI not implemented; test migration scaffold only\n"
left: 78
```

这是预期的红测：`src/main.rs` 仍是空实现，尚未进入任何命令解析、JSON 信封或卸载逻辑；没有网络调用、没有删除真实程序、没有读取个人凭据。

## 格式与编译

- `cargo test --offline --test contract_lifecycle --no-run`：通过。
- 本 lane 的 scoped 格式检查：`cargo fmt --check -- --config skip_children=true` 通过。
- 共享 `tests/contract_support/mod.rs` 不在本 lane 的写入范围，保持未修改；其 rustfmt 收口归整合分支。若本地 worktree 尚未带上整合分支的格式修复，plain `cargo fmt --check` 仍可能报告该共享文件。
