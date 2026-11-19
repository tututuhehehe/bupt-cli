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
| 新版本下载固定 asset，经同一 `Transport`；重定向只允许 GitHub 资产主机或同仓库下载路径 | `cmd_update` 的 asset URL；合同“只作用于获准目标” | `update_apply_replaces_a_verified_fixture_target`、`update_apply_download_and_archive_failures_leave_the_target_untouched` |
| ZIP 由维护中的 `zip` crate 在内存解析，只接受一个安全的 `bupt` 普通文件；拒绝路径穿越、符号链接、重复目标、坏 CRC/截断 | 合同“不扩大删除/写入范围”；不把 ZIP 条目路径写到磁盘 | `stored_entry_is_extracted`、`prefixed_entry_is_extracted`、`deflated_entry_is_extracted`、`corrupt_and_unsafe_archives_are_rejected`、`too_many_entries_are_rejected`、`truncated_and_mutated_archives_never_panic` |
| 目标/发行物必须是本平台架构的原生可执行文件；ARM64 Mach-O、PE x64、ELF 的头部和 load/segment 边界都要通过 | 合同“拒绝错误平台/架构和截断候选” | `native_payload_is_accepted_and_wrong_arch_is_rejected`、`truncated_native_payload_is_rejected`、`update_apply_rejects_scripts_wrong_names_and_non_executables`、`update_apply_download_and_archive_failures_leave_the_target_untouched` |
| 发行物或目标若是 Python launcher（shebang/PyInstaller cookie），拒绝安装/替换 | 合同“不信任 Python launcher”；当前发布物可能仍是 Python 版 | `update_apply_rejects_scripts_wrong_names_and_non_executables`、`update_apply_download_and_archive_failures_leave_the_target_untouched` |
| 目标必须是当前获准的原生可执行文件；脚本、源码、错误文件名、非执行位、符号链接都拒绝；源码/debug/未加发布标记的构建诚实返回未安装 | Python `sys.frozen` 之前的目标判定与 `test_update_install_path_from_source_is_honest`；合同“只更新获准目标” | `update_apply_rejects_a_symlinked_target`、`update_apply_rejects_scripts_wrong_names_and_non_executables`、`update_apply_refuses_a_development_build_even_with_a_target`、`update_apply_source_build_reports_not_installed_without_touching_it` |
| staging 写入/校验通过后才备份；替换前复核目标元数据；替换后校验字节，失败用备份恢复 | `cmd_update` 的备份/替换；`build.sh` 的 staging 原则 | `update_apply_replaces_a_verified_fixture_target`、`update_apply_stage_write_and_backup_failures_preserve_the_target`、`update_apply_failure_after_backup_cleans_the_backup`、`update_apply_replace_failure_cleans_up_the_backup`、`update_apply_corruption_rolls_back_from_the_backup`、`update_apply_restore_failure_keeps_the_backup_and_reports_it` |
| 已有 `bupt.old` 不覆盖；下载、解包、写入、替换失败都不报告已更新 | 合同“不对跳过/失败项报告完成” | `update_apply_requires_the_backup_path_to_be_free`、`update_apply_download_and_archive_failures_leave_the_target_untouched` |
| `--check` 的 note 不再承诺 Windows/无发行物/debug 或源码运行可安装 | `cmd_update` 的 `note`；合同“诚实报告未实现” | `update_notes_do_not_promise_unavailable_installs` |

## 实现（离线与本地路径）

实现文件：`src/lifecycle.rs`、`src/lifecycle/archive.rs`、`src/lifecycle/binary.rs`。只使用注入的 `Transport`/`Scripted`、临时目录、合成 JSON 和假发行物名；没有联网，没有扫描或删除真实安装，也没有对真实已安装程序执行 `update`。替换路径只在单元测试里对临时夹具副本执行。

### `update`

- `--check` / `--repo OWNER/REPO` 保留；`--repo` 只接受严格的 `OWNER/REPO`，URL、空值、额外路径段在联网和写状态前以 usage（退出 2）拒绝。
- `--check` 通过注入的 `Transport` 发起一次 `GET https://github.com/OWNER/REPO/releases/latest`，带 `User-Agent: bupt-cli`；不跟随重定向，不访问 `api.github.com`，不下载 asset。
- 只有 301/302/303/307/308 且 `Location` 是同一仓库的 `https://github.com/OWNER/REPO/releases/tag/<tag>` 才接受；主机、仓库或路径不符、query/fragment、额外路径段、缺失 Location 都以 upstream（退出 5）失败关闭。`<tag> == latest` 保留 Python 的“无版本号”成功结果。
- 成功字段沿用 Python：`current`、`repo`、`latest`（去掉前导 `v`）、`update_available`、`note`。`current` 取编译时的 `CARGO_PKG_VERSION`（当前迁移包为 `0.0.0`），不硬编码 Python 的 `__version__`。无 release（404 且无有效 Location）是成功结果（`update_available=false`），不是错误。`--check` 在 debug/源码/未设置发布标记的构建中 note 说明“当前不是独立发行二进制，未自动安装”，不沿用 Python 会承诺安装的文案。
- 版本比较只取前三个数字段，并按 Python 的短元组规则比较，`1.10.0 > 1.9.0`；相等或更旧的 tag 不报告升级。
- 错误分层：传输失败=4 network；401/403=3 auth；其他 HTTP 状态和畸形/外来重定向=5 upstream。检查不写 config/cache/暂存文件。
- 默认 `update`（无 `--check`）先执行同一 release 检查。无 release、无版本号、已是最新版本都成功返回且不写文件；有新版本才进入安装路径。正式 CLI 已接入独立的 GitHub 传输；只有获准的发布构建才会下载并替换。
- 发行物按平台选择：macOS Apple Silicon 用 `bupt-macos-arm64.zip`，Windows x64 用 `bupt-windows-x64.zip`。Windows 名称沿用 macOS 的命名习惯，发布工作流尚未确认；这是需要整合者确认的歧义点。
- 安装路径从 `https://github.com/OWNER/REPO/releases/download/<tag>/<asset>` 下载 asset，仍走注入的 `Transport`，不访问 `api.github.com`。最多跟随 5 次显式 301/302/303/307/308；`Location` 只允许 HTTPS 的 `github.com` 同仓库下载路径，或 `release-assets.githubusercontent.com` / `objects.githubusercontent.com` / `github-releases.githubusercontent.com`；其他主机、路径穿越、query 以外的畸形值都失败关闭。响应体上限 256 MiB。
- ZIP 不落盘解压，而是用维护中的 `zip` crate（`9.7.1`，`default-features = false`，仅 `deflate-flate2-zlib-rs`）在内存解析：保留 256 MiB 归档/条目上限、4096 条目上限，只接受 stored/DEFLATE、一个安全的 `bupt` 普通文件；由 `zip` 读取器校验 CRC 和解压大小，本层再检查名称安全性、符号链接、目录目标、重复 `bupt`、加密和条目大小。发行物内若是 shebang/PyInstaller launcher，也在写目标前拒绝；当前若仍发布 Python 版 asset，更新会失败关闭而不是把 Python 包装成 Rust 更新。
- 目标只取当前正在运行的 `bupt` / `bupt.exe`，不扫描 PATH/CWD，不依赖 Python 的 `sys.frozen`。目标必须是绝对路径、普通文件、非空、不超过 256 MiB、文件名匹配、（Unix）有执行位、结构完整的本平台原生可执行文件，且不能是符号链接：macOS 要求 ARM64 `MH_EXECUTE` 且 load command/segment 文件范围在候选字节内；Windows 要求 PE32+ x64 和节表范围；其他 Unix 要求对应架构 ELF。错误架构、共享库/非执行文件、截断头部/命令表/段都会拒绝。shebang 脚本、PyInstaller onefile cookie 也拒绝。
- 只有显式编译期发布标记（构建时设置 `BUPT_CLI_RELEASE_BUILD=1`，且非 debug、非 `cfg(test)`）才允许当前运行的程序自动进入替换路径；macOS 路径上，源码、测试、夹具构建和未设置标记的 release 构建都返回 `installed_to=null` 和“未自动安装”的 note，不靠文件名或“路径里有没有 target”判断。单元测试通过私有 target/FS 缝只替换临时夹具副本。
- 替换顺序：在目标同目录创建 staging 目录，写入并读回校验新二进制 → 再次核对目标元数据 → 复制 `bupt.old` 备份并校验字节 → 原子 rename 覆盖目标 → 再次校验目标大小/执行位/字节。任一步失败都保留旧目标；替换后校验失败会用备份恢复；恢复也失败时错误文本明确指出备份位置，不声称已更新。成功时保留 `bupt.old`，显式清理暂存目录（清理失败写进 note，不伪装成已清理），输出 `installed_to`、`backup` 和 `已更新到 <version>` note，与 Python `cmd_update` 的成功字段一致。源码/非独立构建返回 `installed_to=null`，不报告已安装。
- 备份路径固定为目标的 `bupt.old`。若该路径已存在（普通文件、目录或符号链接），更新拒绝覆盖并要求先移动/删除；这是对 Python 会覆盖旧备份的有意保守变化，目的是不碰无关文件。重复更新前需要先处理旧备份。
- Windows 运行时 `.exe` 原位替换仍未实现：`update --check` 的 note 明确说明未自动安装，默认 `update` 在下载前返回 upstream（退出 5）且不改文件。macOS 的实际替换路径已按上述规则接好，等待生产 transport 和真实 release asset 验收。

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
# 218 passed（lifecycle::tests 35 个，lifecycle::archive::tests 6 个，lifecycle::binary::tests 3 个）

cargo test --locked --offline --test contract_lifecycle
# 23 passed; 0 failed
```

新增/相关的单元用例：`repo_must_be_owner_slash_repo`、`release_assets_are_selected_per_supported_platform`、`only_a_regular_bupt_executable_is_a_target`、`removal_plans_only_the_given_executable`、`dry_run_plans_without_removing_the_executable`、`owned_data_requires_recognized_payloads`、`timetable_ownership_requires_known_shape`、`logged_out_credentials_remain_recognizable`、`dry_run_keeps_the_vetted_directory`、`mixed_directory_is_rejected_before_removal`、`late_unknown_file_survives_execution`、`changed_owned_file_survives_execution`、`replaced_directory_survives_execution`、`uninstall_note_distinguishes_completion_from_skips_and_keep_data`、`dangerous_roots_are_refused_without_following_aliases`、`update_check_reports_a_newer_release_without_side_effects`、`update_check_reports_missing_release_and_no_upgrade`、`update_check_reports_a_redirect_without_a_version`、`release_location_must_name_the_same_repository_tag`、`update_check_rejects_a_foreign_redirect`、`release_check_layers_transport_and_http_failures`、`version_ordering_does_not_promote_older_tags`、`update_apply_replaces_a_verified_fixture_target`、`update_apply_rejects_a_symlinked_target`、`update_apply_rejects_scripts_wrong_names_and_non_executables`、`update_apply_refuses_a_development_build_even_with_a_target`、`update_apply_requires_the_backup_path_to_be_free`、`update_apply_download_and_archive_failures_leave_the_target_untouched`、`update_apply_stage_write_and_backup_failures_preserve_the_target`、`update_apply_failure_after_backup_cleans_the_backup`、`update_apply_replace_failure_cleans_up_the_backup`、`update_apply_corruption_rolls_back_from_the_backup`、`update_apply_restore_failure_keeps_the_backup_and_reports_it`、`update_apply_source_build_reports_not_installed_without_touching_it`、`update_notes_do_not_promise_unavailable_installs`、`stored_entry_is_extracted`、`prefixed_entry_is_extracted`、`deflated_entry_is_extracted`、`corrupt_and_unsafe_archives_are_rejected`、`too_many_entries_are_rejected`、`truncated_and_mutated_archives_never_panic`、`native_payload_is_accepted_and_wrong_arch_is_rejected`、`truncated_native_payload_is_rejected`、`python_launchers_are_not_native_candidates`。

此前因 identity / homework 尚未接入而失败的三条 lane 用例（`status_is_one_compact_json_document_with_schema`、`pretty_changes_layout_without_changing_values`、`homework_uses_the_canonical_default_entry`）在当前基线已通过；本轮 lane 合同测试全绿。

## 暂不能离线验证（需要已约定的测试缝）

以下场景仍未完成或需要整合者先确定最小测试缝，不能用假断言或猜测内部 API 代替。网络、真实 GitHub、真实已安装二进制和真实更新动作均不进入本轮测试。

1. **release 检查：离线已覆盖，真实 GitHub 未验收**
   - Python：`_release_tag`、`test_update_uses_release_redirect_not_the_api`、`test_release_tag_reads_the_redirect`、`test_update_reports_missing_release`。
   - 已覆盖：注入 `Transport`/`Scripted` 下的 GET、不跟随重定向、同仓库 tag 路径校验、无 release、畸形/外来重定向、版本边界、network/upstream/auth 分层和零文件副作用。
   - 已接生产只读传输；仍须查询确实有发布版本的仓库，不能把离线用例当作线上验收。

2. **更新 staging / 原子替换 / 失败回滚**
   - Python：`cmd_update`、`test_update_replaces_a_frozen_binary`、`test_update_install_path_from_source_is_honest`；`build.sh` 的“先打包到 staging、冒烟、再原子替换，失败不覆盖旧二进制”。
   - 已覆盖：注入 transport + FS 下的 asset 下载、重定向限制、维护中 `zip` crate 的严格选择、原生架构/结构校验、staging 写入/读回、备份、替换后校验、失败清理和恢复。目标始终是临时夹具副本，不碰真实 `bupt`。
   - 仍缺：真实 macOS Apple Silicon release asset 的 zip 形态/权限/`--version` 冒烟和实际替换验收；Windows 运行时 `.exe` 替换；旧 Python 安装的现场迁移策略需要整合者决定。当前实现会拒绝 shebang/PyInstaller 目标和发行物；源码运行会诚实返回 `installed_to=null` 而不是安装，因此发布仓库在切换到 Rust 原生 asset 之前，真实更新应预期失败关闭，不能拿旧 Python release 验收成功。

3. **`update` 的网络/上游错误分层**
   - Python：`from_campus_error`、`test_http_status_network_error_maps_upstream`、`test_http_401_network_error_maps_auth`（同属错误合同）。
   - release 检查和 asset 下载已用可注入传输覆盖：传输失败=4 network，其他 HTTP 4xx/5xx=5 upstream，401/403=3 auth；错误路径只产生一个 stderr JSON、stdout 为空，成功路径只写 stdout JSON。
   - 仍缺：真实校外网络下的超时/断网和 scoped production transport 验收；不能用真实网络假装通过。

4. **凭据脱敏覆盖**
   - Python：`test_cli_error_messages_are_redacted`。
   - 缺口：需要 identity lane 提供的凭据注入/错误回显测试缝；不能在测试里使用真实密码。
   - 待验证：合成密码出现在上游错误文本里时被替换为 `[REDACTED]`；日志、help 和错误信封不携带秘密。

5. **卸载的 PATH/CWD 扫描范围**
   - 当前实现明确不扫描 `PATH`/CWD；只把当前正在运行的已验证可执行文件作为候选。用例把无关可执行文件/源码放在显式临时 `PATH` 与 CWD，并断言它不会被删除。
   - 若后续改为扫描 PATH/CWD，必须把被拒绝的候选放进 `skipped`，并重新审视“只作用于获准目标”的合同。

6. **在线更新与 Windows swap（暂缓、待实测）**
   - `update --check`、asset 下载、ZIP 选择、macOS 原位替换和回滚均已用注入 transport/FS 离线覆盖。正式传输已接入，尚未验证真实 Rust 发行物。
   - Windows 原位替换/运行中文件锁行为未实现；Windows 上的 `update --check` 会明确说明未自动安装，默认 `update` 在下载前返回 upstream（退出 5）且不改文件。不得用 live network/publish 假装通过。
   - macOS Apple Silicon 与 Windows x64 的真实 release 可执行文件、权限位、替换行为和 `--version` 冒烟测试需要两个平台的实际运行；Windows 运行中二进制的替换仍未实现。

## 整合者接线要求

- **依赖**：本线在 `Cargo.toml`/`Cargo.lock` 增加 `zip = { version = "9.7.1", default-features = false, features = ["deflate-flate2-zlib-rs"] }`；只启用 Rust `zlib-rs` 的 DEFLATE 读写，不启用 AES、bzip2、deflate64、lzma、ppmd、zstd、xz 或 zopfli。`zip` 仍会带入其平台抽象依赖（`libc`/`nix`/`windows-sys` 等），但没有系统压缩库或 C 构建脚本。若整合者要求更低依赖面，需要另行批准替代 crate/feature；本线不自行扩大包集合。
- **发布构建标记**：只有 `BUPT_CLI_RELEASE_BUILD=1 cargo build --release` 生成的二进制才允许自动替换当前程序；未设置标记的 release、debug、测试和夹具构建都不自动安装；macOS 可用路径返回 `installed_to=null`。
- **生产传输**：`--check` 只允许版本元数据 GET；默认 `update` 另允许固定平台包路径和已知 GitHub 资产主机，安装流程逐跳校验。无凭据、无 curl 自动跳转；资产网络响应上限 64 MiB，解包后上限 256 MiB。版本查询正向 CLI 用例只用注入传输。
- **Windows**：需要单独的运行中 `.exe` 替换/退出后 swap 策略和双平台验收；在本轮不能声称已实现。Windows 候选结构校验已按 PE32+ x64 实现，但没有运行中替换。

## 历史红测记录

测试迁移阶段的 23 个用例曾在脚手架 `src/main.rs` 上全部失败，失败统一为：

```text
unexpected exit code; stdout=""; stderr="Rust CLI not implemented; test migration scaffold only\n"
left: 78
```

现在 lifecycle 的本地实现已接入，上述失败只剩其他功能线的待接用例（见“本轮测试结果”）。

## 格式与编译

- `cargo test --locked --offline --test contract_lifecycle --no-run`：通过。
- `cargo fmt --check` 通过。
- `Cargo.toml`/`Cargo.lock` 只增加经批准的 `zip` 依赖及其传递依赖；共享 `tests/contract_support/mod.rs`、`src/cli.rs`、`src/transport.rs` 未修改。
