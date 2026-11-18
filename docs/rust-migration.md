# Rust 迁移合同

Python 对照：`d7c35ef`。该版本的代码和离线测试用来查语义，不作为 Rust 运行依赖。本文列出的有意变化优先于旧测试；发现其余公开行为冲突时交给整合者，别悄悄调整预期。

## 公开入口

最多两级。`login` / `status` / `logout`；`courses`；`homework`（默认列表）/ `show` / `download` / `submit` / `verify`；`schedule`（默认本周）/ `update` / `calendar` / `export`；`exams`；`scores`；`classrooms`；`library search` / `hold` / `ebook`；`electricity` / `buildings`；`notices` / `show`；`card`；`diagnose`；`update`；`uninstall`；`help [命令]`。不提供 `homework list`、`schedule exams` 或 `probe` 作为旧别名。`help`、`--help`、`--version` 保持文本；非交互业务输出默认一个 JSON 文档，分别在 stdout（结果）和 stderr（错误），带 `schema: "bupt.v1"`；`--pretty` 不改变值。保留能沿用的字段、参数和退出码。非法参数在读写业务状态、联网或启动浏览器前拒绝。

## 行为验收

- **身份**：`login` 验证校园凭据，登记显式选择的本科/研究生身份，不预热具体服务。变更凭据时不能拿旧会话证明新密码有效，失败时保留上一次有效登记和会话。密码暂存本地 JSON：macOS 用户私有权限，Windows 没有 POSIX `0600`，要验证等价的用户访问边界。Cookie/token 同样敏感；测试、日志和错误不得携带真实秘密。首次启用可读取旧的账号配置；只复用归属可验证且兼容的旧会话；`logout` 清掉本地密码和会话，保留课表与设置。
- **续期**：服务按需建立会话；只有明确过期才允许一次自动续期。暂时断网、拒绝密码、限流不是续期信号。研究生不触发本科教务认证；学号或身份切换不复用另一所有者的状态。`status` 只读本机状态，`diagnose` 能联网但不提交密码、不写本地会话；没有可用会话时标注跳过。
- **教务**：`schedule` / `calendar` / `export` 只读当前学号及身份的可用课表快照，不联网、不启动浏览器；`schedule update` 是刷新入口。不能确认旧快照归属时提示刷新。`exams` 按身份选数据源，输出共同外壳，不伪造考试日期、场次、地点。不经过本科教务的研究生路径可以依赖已安装的 Chrome，不捆绑浏览器；macOS 和 Windows 均需验证。导出文件成功不因打开系统文件管理器失败变成导出失败。
- **作业**：没有 `--i-confirm-submit` 不上传、不提交，`--dry-run` 同样不写入。兼容 `6/7` 表示可能已提交但无法确认；先只读核验，绝不自动重发提交请求。`verify` 比对正文/附件时可建立访问会话，但不写校园作业数据。
- **分发**：macOS Apple Silicon 与 Windows x64 分别发布一个原生可执行文件。迁移新版不静默覆盖旧 Python 版。更新和卸载只作用于获准的目标；卸载可清理自己的课表，不扩大删除范围，不对跳过项报告为已删除。

## 初轮：先迁测试

1. 从任务指定的 Python 测试找输入、边界和预期，再对照上面的有意变化。每条迁移或变更至少对应一个 Rust 用例（或记为待实现的集成场景并解释原因）。不搬 Python 私有函数调用和数据结构。
2. 只写任务拥有的 `tests/contract_<lane>.rs`、`tests/fixtures/contracts/<lane>/` 和场景清单 `docs/migration-tests/<lane>.md`。清单标注 Python 测试名或代码位置、覆盖的 Rust 用例，以及暂不能验证的边界。共享 harness 和依赖变更由整合者负责。
3. Rust 集成测试使用共享 `mod contract_support;`：`Sandbox::new()` 隔离家目录、配置和缓存；`.run(&[...])` 捕获进程输出；`.stdout_json()` / `.stderr_json()` 解析单个文档。按语义断言退出码、输出流、字段、禁止的副作用；对现场联网、真实密码、已安装浏览器不可依赖。如基础设施不足，在清单写明所需的最小测试缝，不用永远通过的假测试代替。
4. 交付一个测试提交。`cargo fmt --check` 和 `cargo test --offline --test contract_<lane> --no-run` 要通过；运行目标测试，预期失败应准确指出空实现缺少的行为，不接受编译错误、真实联网、`#[ignore]` 或 `#[should_panic]` 掩盖失败。报告提交号、编译命令、失败分组和未覆盖项，停在测试阶段。
5. 整合者审查用例能否捕获负例、是否符合合同、夹具是否脱敏、是否越界；复跑并合并测试后才派实现。各模块只改分配文件，共享入口、依赖、锁文件及跨平台发布由整合者收口。实现审查需通过格式、Clippy、全套 Rust 离线测试和两平台实际运行；校园网上游验收单独记录为待实测。最后从验收过的命令和输出重写中英文 README、使用说明与项目 CLI Skill。
