# bupt — 北邮校园命令行工具

[English](README.en.md) | **简体中文** | [使用说明](使用说明.md)

北京邮电大学校园服务的非官方命令行客户端，**原生 Rust 单文件二进制**，面向人和 agent 使用：每次调用输出一行 JSON（`schema: "bupt.v1"`），所有状态集中在 `~/.bupt-cli/`（目录 `0700`，文件 `0600`）。

**MIT 开源。** 源码里不含任何账号、密码或会话，首次使用跑一次 `bupt login` 配置你自己的。

> 非官方软件，不是生产系统。校园接口随时可能变化。请遵守学校网络使用规定，不要高频抓取。

## 功能

- **统一身份认证（CAS）登录**：交互式保存账号，一套密码通用于北邮各系统；只读命令按需建立会话，不重复提交密码
- **云邮教学空间（UCloud）**：课程列表、作业列表/详情/附件下载、提交前预演与只读核验；真实提交需 `--i-confirm-submit`
- **研究生教务（yjxt）**：学期课表、教学日历、考试安排、iCalendar 导出，全程**直连 HTTPS，不需要浏览器**
- **本科教务（移动 JWGL）**：课表、考试、成绩、空教室
- **图书馆**（`opac.bupt.edu.cn`）：书目检索、馆藏位置、电子版链接，**匿名可用**
- **信息门户**（`my.bupt.edu.cn`）：通知公告、饭卡余额
- **宿舍电费**（`app.bupt.edu.cn`）：沙河 / 西土城楼栋与余额
- **`bupt diagnose`**：公开入口与会话的实时健康矩阵
- **`bupt update --check` / `bupt uninstall`**：只读更新检查与保守卸载

## 实测状态（macOS Apple Silicon，2026-11）

| 场景 | 状态 | 证据 |
|---|---|---|
| `login`（研究生） | ✅ 实测 | 单次密码提交建立 CAS 会话 |
| `courses` / `homework`（列表、`--course`、`--new`、`show`、`download`、`verify`、`submit --dry-run`） | ✅ 实测 | 11 门课；真实附件下载成功；已交作业只读核验通过 |
| `schedule` / `schedule update` / `calendar` / `export`（研究生） | ✅ 实测 | 20 教学周、6 门课；ICS 96 个事件与文件一致；刷新约 0.8 秒 |
| `exams`（研究生） | ✅ 实测 | 查询成功返回 0 条（当学期无考试） |
| `library search` / `hold` / `ebook` | ✅ 实测 | 匿名；检索 3 条、馆藏 10 条 |
| `electricity buildings --area xitucheng` | ✅ 实测 | 12 栋宿舍楼 |
| `diagnose` | ✅ 实测 | 14 个端点：13 成功、1 跳过、0 失败 |
| `update --check` / `uninstall --dry-run` / `logout` | ✅ 实测（隔离副本） | 不安装、不误删 |
| 门户 `notices` / `card` | ⛔ 环境受限 | 测试机到 `my.bupt.edu.cn` HTTPS 不通，未验收 |
| `electricity --area shahe` | ⛔ 上游异常 | 接口返回业务错误 `e=1`，非本机网络问题 |
| 真实作业提交 | ⏳ 待验收 | 代码已接线且门禁开启，等待指定目标作业实测 |
| Windows x64 | ⏳ 待验收 | 见下方「平台支持」 |

## 平台支持

当前发布 **macOS Apple Silicon** 单文件二进制。Windows x64 是既定目标（凭据存储、系统 `curl.exe` 调用、自替换已留好分支），但尚未实机验收；需要 Windows 的用户请等发布说明确认后再用。

## 安装

从 [Releases](https://github.com/tututuhehehe/bupt-cli/releases) 下载 `bupt-macos-arm64.zip`：

```bash
unzip bupt-macos-arm64.zip && chmod +x bupt-macos-arm64/bupt
bupt --version        # 期望输出 bupt 0.6.0
```

把二进制放进 PATH 即可（例如本仓库根目录；`~/.zshrc` 已把 `moon_cli_or_tool/*` 全部加入 PATH 的机器直接复制进去就行）。

自己构建（产物进 `dist/bupt-macos-arm64.zip`，构建前先跑离线冒烟）：

```bash
./build-rust.sh          # macOS Apple Silicon
```

二进制只依赖系统 `curl`，**不需要 Python、不需要浏览器、不读你的 Chrome 配置**。

## 配置账号

```zsh
$ bupt login --role graduate
学号：2026xxxxxx
密码（输入时不显示）：
```

- 凭据只存 `~/.bupt-cli/credentials.json`（权限 `0600`）。**不再支持 `BUPT_USERNAME` / `BUPT_PASSWORD` / `BUPT_ROLE` 环境变量**——环境里只有这些变量时命令会明确报 auth。
- `role` 取 `graduate` 或 `undergraduate`，决定教务命令走哪条数据源。
- `~/.bupt-cli/session.json` 等同于凭据（含 Cookie/token，不含密码），请当作机密保管。
- `bupt status` 只显示脱敏元数据；`bupt logout` 删除凭据与会话，保留课表快照与宿舍设置。

### 宿舍电费默认值

`bupt electricity` 不带参数时使用**示例占位符**（西土城 学一楼 101），查询前请指定自己的宿舍：

```bash
bupt electricity --building 学5 --room 416
bupt electricity buildings --area shahe        # 或 xitucheng
```

楼/房间/楼层独立解析，优先级：**命令行参数 > 环境变量（`BUPT_DORM_BUILDING/ROOM/FLOOR`）> `~/.bupt-cli/config.json` > 占位符**。传 `--building` 必须同时传 `--room`。

其他可用环境变量：

| 变量 | 含义 |
|---|---|
| `BUPT_CLI_CONFIG_DIR` / `BUPT_CLI_CACHE_DIR` | 覆盖配置/缓存目录（默认 `~/.bupt-cli`） |
| `BUPT_CLI_REPO` | `update` 的默认 GitHub 仓库（`OWNER/REPO`，可用 `--repo` 覆盖） |

## 命令一览

所有命令向 stdout 输出紧凑 JSON，任意位置加 `--pretty` 缩进。错误走 stderr：`{"schema":"bupt.v1","ok":false,"error":{"type":"...","message":"...","hint":"..."}}`，退出码分层：

| 退出码 | 层 | 含义 |
|---|---|---|
| 0 | — | 成功 |
| 2 | usage | 参数/本地配置非法，发生在联网之前 |
| 3 | auth | 无凭据、会话过期或被拒；提示 `bupt login` |
| 4 | network | 连接失败/超时 |
| 5 | upstream | 校园接口返回异常或结构变化 |
| 6 | — | 提交结果不明（绝不自动重发） |
| 7 | — | 提交后核验失败/出错 |

| 命令 | 说明 |
|---|---|
| `bupt login --role graduate\|undergraduate [--user ID] [--password PASS]` | 注册并验证账号；`--role` 必填，学号/密码缺省时交互式询问 |
| `bupt status` | 本地账号与会话状态（脱敏，不联网） |
| `bupt logout` | 删除凭据与会话，保留课表与设置 |
| `bupt diagnose [--only public,cas,ucloud]` | 服务可用性矩阵；不提交密码、不写会话 |
| `bupt courses` | 云邮教学空间全部课程 |
| `bupt homework [--undone] [--course ID] [--new DAYS] [--limit N]` | 作业列表（默认全部，按截止时间排序） |
| `bupt homework show ID [--course SITE_ID]` | 作业详情：提交状态、正文、附件 |
| `bupt homework download ID [--dir DIR] [--force]` | 下载教师附件（≤8 MiB/个，只写指定目录） |
| `bupt homework submit ID --dry-run` | 预演：读取详情与提交状态，展示将上传/提交的内容，零写入 |
| `bupt homework submit ID --i-confirm-submit [--file PATH]… [--text …]` | 真实上传并提交**恰好一次**；失败/断连不自动重发，只提示用 `verify` 核查 |
| `bupt homework verify ID [--resource-id RID]… [--text TEXT]` | 只读核验提交是否可见；不带参数只查「有没有交」 |
| `bupt schedule [--today\|--tomorrow\|--date YYYY-MM-DD\|--week N] [--raw]` | 读本地课表快照（毫秒级）；学期内不自动重抓 |
| `bupt schedule update` | 主动刷新快照；研究生走直连 HTTPS（约 0.8 秒），本科走移动教务 |
| `bupt schedule calendar` | 教学周 → 日期对照 |
| `bupt schedule export [--out PATH]` | 导出 iCalendar（默认 `~/Downloads/bupt-<学期>.ics`） |
| `bupt exams` | 考试安排，按身份路由数据源；空结果与解析失败严格区分 |
| `bupt scores` / `bupt classrooms [--campus 01\|04]` | 本科成绩 / 空教室 |
| `bupt library search KEYWORD [--page N] [--page-size N] [--location CAMPUS] [--with-ebook]` | 匿名书目检索 |
| `bupt library hold RECCTRLID` / `bupt library ebook RECCTRLID` | 馆藏位置 / 电子资源链接 |
| `bupt electricity [--building B] [--room R] [--floor F] [--threshold N]` | 宿舍电费余额；传 `--building` 必须同时传 `--room` |
| `bupt electricity buildings [--area shahe\|xitucheng]` | 宿舍楼列表 |
| `bupt notices [--type notice,news,rules,ways] [--count N]` / `bupt notices show URL` | 门户通知（需校园网络路由） |
| `bupt card` | 饭卡余额 |
| `bupt update --check [--repo OWNER/REPO]` | 只读检查新版本；安装只对带发布标记的 macOS 构建开放 |
| `bupt uninstall [--keep-data] [--dry-run]` | 保守卸载：拒绝危险根目录、symlink、混合目录 |

保留字段、删除别名：`probe`→`diagnose`、`homework list`→`homework`、`schedule exams`→`exams`、`status --check`、`login --jwgl` 不再存在。

## 隐私与安全设计

- 凭据与会话文件全部 `0600`（所在目录 `0700`），写入走「临时文件 + 原子改名」，拒绝 symlink 与过宽权限
- 所有请求走系统 `curl`（参数经 stdin 配置，命令行不出现密码/票据/Cookie）
- 校外跳转逐个校验：CAS/教务/门户/电费各自只接受已验证的 HTTPS 主机；站点宣告的 HTTP 一律升级，绝不降级
- 研究生教务**不启动浏览器**、不执行站点脚本、不读取浏览器配置
- 作业提交默认只读；`--dry-run` 零写入；确认后至多一次提交，结果不明时退出 6 并指向只读核验
- 卸载有护栏：只删「bupt 自己的」数据，混合目录整目录跳过并如实报告

## 已知限制

- 门户 `notices`/`card` 依赖校园网到 `my.bupt.edu.cn` 的路由，校外或路由不通时报 network 4
- 沙河电费楼栋接口当前返回上游业务错误（`e=1`），西土城正常
- 真实提交与 Windows 版本尚未完成实机验收（见实测状态表）

## 与 Python 版（`d7c35ef`）的差异

- 单文件原生二进制取代 PyInstaller 打包；不再需要 Python 运行时
- 研究生教务从「无头 Chrome/CDP」改为直连 HTTPS，快约 4 倍，零浏览器依赖
- 凭据只从 `credentials.json` 读取，环境变量回退已移除
- `homework` 默认即列表；`exams` 按身份自动路由；`probe` 更名 `diagnose` 且不提交密码
- 版本从 0.5.x 起跳至 **0.6.0**；旧 Python 版完整保留在 git 历史 `d7c35ef`

## 致谢

接口知识来自社区整理与上游项目（[campus-api](https://github.com/Windy-Dance/campus-api)、[BUPT-API-Collection](https://github.com/uarix/BUPT-API-Collection)、[bupt-api](https://github.com/wzekin/bupt-api)、[bupt-api-collected](https://github.com/Yokumii/bupt-api-collected)），移植中逐项重新实测。MIT 许可见 [LICENSE](LICENSE)。
