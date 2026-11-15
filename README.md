# bupt — 北邮校园命令行工具

[English](README.en.md) | **简体中文**

北京邮电大学校园服务的非官方命令行客户端，面向人和 agent 使用：每次调用输出一行 JSON，所有状态集中在 `~/.bupt-cli/`（权限 `0600`）。

**MIT 开源。** 源码里不含任何账号、密码或会话，首次使用跑一次 `bupt login` 配置你自己的。

> 非官方软件，不是生产系统。校园接口随时可能变化。请遵守学校网络使用规定，不要高频抓取。

## 功能

- **统一身份认证（CAS）登录**：交互式保存账号，一套账号密码通用于北邮各系统
- **云邮教学空间（UCloud）**：课程列表、作业列表/详情/附件下载；提交需二次确认
- **图书馆**（`opac.bupt.edu.cn`）：书目检索、馆藏位置、电子版链接。**匿名接口**，本科生研究生都能用
- **研究生系统**（`yjxt.bupt.edu.cn`）：学期课表、教学日历、考试安排，以及 **`bupt schedule export`** 导出带真实日期的日历文件
- **信息门户**（`my.bupt.edu.cn`）：通知公告、饭卡余额
- **宿舍电费**（`app.bupt.edu.cn`）：沙河 / 西土城
- **`bupt probe`**：全部接口的实时健康矩阵
- **`bupt update` / `bupt uninstall`**：自我更新与卸载

每个接口的实测状态与证据见 [API_STATUS.md](API_STATUS.md)。

## 适用范围（本科生 vs 研究生）

2026-11 在研究生账号上实测：

| 功能 | 本科生 | 研究生 |
|---|---|---|
| 云邮教学空间（`courses`、`homework *`） | ✓ | **✓** |
| 信息门户（`notices`、`card`） | ✓ | **✓** |
| 图书馆（`library *`） | ✓ | **✓** |
| 宿舍电费 | ✓ | ✓ |
| 研究生系统（`grad *`） | ✗ | **✓** |
| 本科教务（`schedule`、`exams`、`scores`、`classrooms`） | ✓ | **✗ — 账号在该系统中不存在** |

`schedule` / `exams` / `scores` / `classrooms` 访问的是**本科**教务系统（`jwgl.bupt.edu.cn` / `jwglweixin.bupt.edu.cn`），研究生学号在里面**根本不存在**，所以这些命令对研究生账号一定失败。

**不要因为报"密码错误"就反复换密码重试——连续失败会锁号。** 报错提示里已经明确说明这一点。

## 安装与运行

### 方式一：源码运行（开发用）

项目根目录的 `bupt` 就是唯一入口。它把 `PYTHONPATH` 指向 `src/` 和 `.deps/`，直接跑 CPython——**没有虚拟环境、没有 editable 安装、没有 `.pth` 指针**，所以不会出现"包莫名消失"。

```bash
cd /path/to/bupt-cli
./bupt --help
```

| 内容 | 位置 |
|---|---|
| 源码 | `src/`（直接加载，不安装） |
| httpx + lxml | `.deps/`（一次性装好，与任何 venv 无关） |
| 解释器 | uv 管理的 CPython 3.14（只读引用） |

`.deps/` 万一丢了，重装一次：

```bash
uv pip install --python ~/.local/share/uv/python/cpython-3.14-macos-aarch64-none/bin/python3.14 \
  --target .deps 'httpx>=0.27.0' 'lxml>=5.0.0'
```

### 方式二：单文件二进制（macOS arm64，推荐）

从 [Releases](https://github.com/tututuhehehe/bupt-cli/releases) 下载 `bupt-macos-arm64.zip`：

```bash
unzip bupt-macos-arm64.zip && chmod +x bupt
./bupt login
```

或者自己构建：

```bash
./build.sh             # -> dist/bupt（约 16 MB，自包含）
./dist/bupt --version
```

二进制只链接系统 `libSystem` / `libz`，**不需要装 Python**。所有状态仍在 `~/.bupt-cli/`，二进制本身不携带任何数据。

`dist/` 是构建产物，可随时删（`./build.sh --clean`）。

**本仓库不会把自己装进系统。** 想放到 PATH 上就自己拷：

```bash
cp dist/bupt ~/.local/bin/bupt && chmod 755 ~/.local/bin/bupt
sudo cp dist/bupt /usr/local/bin/bupt     # 或系统级
```

卸载：

```bash
bupt uninstall             # 删除 PATH 上的 bupt，同时删除 ~/.bupt-cli
bupt uninstall --keep-data # 保留 ~/.bupt-cli（账号密码与会话）
bupt uninstall --dry-run   # 只显示会删什么
```

`uninstall` 有安全护栏：拒绝删除 `/`、家目录、当前目录及其上级，且要求目标目录里只有 bupt 自己的数据文件，避免环境变量写错时误删。

## 配置账号

凭据从环境变量或 `~/.bupt-cli/credentials.json` 读取。`bupt login` 会交互式询问并写入后者（权限 `0600`）。

```zsh
$ bupt login
配置文件: /Users/you/.bupt-cli/credentials.json
学号：2026xxxxxx
密码（输入时不显示）：
身份（1=研究生 2=本科生）：1
```

优先级：

1. 环境变量（最高）

   ```bash
   export BUPT_USERNAME=2026xxxxxx
   export BUPT_PASSWORD='...'
   ```

2. `~/.bupt-cli/credentials.json`（权限 `0600`）

   ```json
   {
     "username": "2026xxxxxx",
     "password": "统一身份认证密码",
     "role": "graduate"
   }
   ```

   `role` 取 `graduate` 或 `undergraduate`，决定哪些教务命令可用。

非交互模式（脚本用；密码会进 shell 历史，不推荐）：

```bash
bupt login --user 2026xxxxxx --password '...' --role graduate
```

可选环境变量：

| 变量 | 含义 |
|---|---|
| `BUPT_DORM_BUILDING` / `BUPT_DORM_ROOM` / `BUPT_DORM_FLOOR` | `bupt electricity` 的默认宿舍 |
| `BUPT_CLI_CONFIG_DIR` | 覆盖配置目录（默认 `~/.bupt-cli`） |
| `BUPT_CLI_CACHE_DIR` | 覆盖会话缓存目录（默认 `~/.bupt-cli`） |
| `BUPT_MAX_CAS_LOGINS` | 单进程 CAS 登录上限（默认 5），防止反复登录锁号 |

密码、Cookie、UCloud token 不会被任何命令打印。`bupt status` 只显示脱敏元数据（域名/条数、是否存在、过期时间）。**`~/.bupt-cli/session.json` 等同于凭据**：里面有会话 Cookie 和 token（不含密码），请当作机密保管。

### 宿舍电费的默认值

`bupt electricity` 不带参数时用的是**示例占位符**（西土城 学一楼 101），不是任何真实用户的宿舍。查询前请改成自己的：

```bash
bupt electricity --building 学一楼 --room 101 --pretty
bupt electricity --building SHAHE_A --room 102 --pretty
bupt electricity buildings --area xitucheng --pretty   # 列出西土城宿舍楼
bupt electricity buildings --area shahe --pretty       # 列出沙河宿舍楼
```

每个字段（楼/房间/楼层）独立解析，顺序为：**命令行参数 > 环境变量 > `~/.bupt-cli/config.json` > 内置占位符**

```json
{"dorm": {"building": "学一楼", "room": "101", "floor": "1"}}
```

- 输出里的 `"source"`（`flag` / `env` / `config` / `default`）表示**房间号**来自哪里
- 楼名支持 `学五楼` / `学5楼` / `学5` / `5` 和沙河的 `SHAHE_A` / `沙河校区雁北园A楼`，校区由楼名自动推断
- 传了 `--building` 就必须同时传 `--room`，否则退出 2
- `config.json` 写坏会阻塞查电（退出 2），但**不影响** `buildings` 列楼

## 命令一览

所有命令向 stdout 输出紧凑 JSON，任意位置加 `--pretty` 缩进。错误走 stderr，格式 `{"ok":false,"error":{"type":"...","message":"...","hint":"..."}}`。

| 命令 | 说明 |
|---|---|
| `bupt login [--user ID] [--password PASS] [--role ROLE] [--force] [--jwgl]` | 保存账号到 `~/.bupt-cli/credentials.json` 并登录。无参数时交互式询问。`--jwgl` 额外尝试移动教务登录（仅本科） |
| `bupt status [--check]` | 显示缓存会话是否有效（不含机密）。`--check` 做只读在线检查 |
| `bupt logout` | 删除 `~/.bupt-cli/session.json` |
| `bupt courses` | 当前云邮教学空间课程 |
| `bupt homework list [--undone] [--course ID] [--new DAYS] [--limit N]` | 作业列表，按截止时间排序。`--undone` 只看未交；`--new N` 只看最近 N 天发布的 |
| `bupt homework show ID [--course SITE_ID]` | 作业详情：提交状态、正文全文、附件地址 |
| `bupt homework download ID [--dir DIR] [--force]` | 下载教师附件到 `DIR` |
| `bupt homework submit ID [--file PATH]… [--text …] [--dry-run \| --i-confirm-submit] [--no-verify]` | 上传/提交。**默认绝不自动提交；不带 `--i-confirm-submit` 时连文件都不上传** |
| `bupt homework verify ID [--resource-id RID]… [--text TEXT]` | 只读核验提交是否可见，不调用任何写接口。不给参数只查"有没有交"，给了才精确比对附件/正文 |
| `bupt library search KEYWORD [--page N] [--page-size N] [--location CAMPUS] [--with-ebook]` | 图书馆书目检索（匿名）。`--with-ebook` 标出有电子版的记录 |
| `bupt library hold RECCTRLID` | 某本书的各馆藏位置：部门、条码、索书号、状态 |
| `bupt library ebook RECCTRLID` | 某本书的电子资源链接 |
| `bupt schedule` | **本周课表**（读本地缓存，毫秒级）。`--today` / `--tomorrow` / `--date` / `--week N` 查指定日期；`--raw` 看原始课程网格 |
| `bupt schedule update [--force]` | 刷新 `~/.bupt-cli/timetable.json`。研究生走浏览器（4-8 秒），本科生走移动教务 API |
| `bupt schedule export [--out PATH]` | 导出 iCalendar 文件（默认 `~/Downloads/bupt-<学期>.ics`），每次上课一个事件 |
| `bupt schedule calendar` | 教学周 → 日期的对照表 |
| `bupt schedule exams` | 考试安排（仅研究生系统，常为空） |
| `bupt electricity [--building B] [--room R] [--floor F] [--threshold N]` | 宿舍电费余额 |
| `bupt electricity buildings [--area shahe\|xitucheng]` | 列出某校区的宿舍楼 |
| `bupt notices [--type notice,news,rules,ways] [--count N] [--page P]` | 门户通知。校外常不可达 |
| `bupt notices show URL` | 通知正文 |
| `bupt card` | 饭卡余额 |
| `bupt schedule [--week N] [--today] [--date YYYY-MM-DD] [--source auto\|mobile\|dom]` | 本科课表（实验性，研究生不可用） |
| `bupt exams` / `bupt scores [--semester NAME]` / `bupt classrooms [--campus 01\|04]` | 本科教务（实验性，研究生不可用） |
| `bupt probe [--only GROUPS]` | 接口健康矩阵 |
| `bupt update [--check] [--repo OWNER/REPO]` | 与最新 release 比对并原位替换二进制（保留 `bupt.old`） |
| `bupt uninstall [--keep-data] [--dry-run]` | 卸载 |
| `bupt help [command]` | `--help` 的别名，`bupt help grad` 看子命令选项 |

### 会话复用与锁号安全

`bupt login` 会缓存 Cookie 和 token，后续命令复用，只在必要时重新登录。CAS 有登录次数上限，单进程内默认 5 次（`BUPT_MAX_CAS_LOGINS`）。

**不要在短时间内反复 `login`。** 报错时先看 `type` 和 `hint`，不要盲目重试。

### 提交作业

工具默认**不会自动提交**：

```bash
# 1. 先 dry-run：只校验参数、显示将要提交的内容，不上传不提交
bupt homework submit ID --file ./答案.pdf --text '说明' --dry-run --pretty

# 2. 你自己核对预览内容，确认后才加 --i-confirm-submit
bupt homework submit ID --file ./答案.pdf --text '说明' --i-confirm-submit --pretty
```

- **每次真正提交都会发送完整正文和全部附件，并覆盖上一次提交。** 有多个文件时每次都要全部带上
- 提交后工具会做只读核验：`verified: true` + 退出 0 表示确认；退出 6 表示未能确认；退出 7 表示核验过程本身失败。这两种情况请用 `bupt homework show` 或 `bupt homework verify` 复查，**不要盲目重交**
- 没有 `--i-confirm-submit` 时工具会拒绝真正提交（退出码 2）

核验逻辑：提交成功后用只读接口再确认一次——`submit-view`（附件集合、正文）和 `student/list`（`submitTime`）。最多尝试 2 次，每次前等待 5 秒；`--no-verify` 跳过核验。

底层接口：

- 上传 `POST https://apiucloud.bupt.edu.cn/blade-source/resource/upload/biz`
- 提交 `POST https://apiucloud.bupt.edu.cn/ykt-site/work/submit`
- 查看已交 `GET …/ykt-site/work/submit-view?assignmentId=…`
- 作业详情 `/ykt-site/work/detail` **不含**学生自己的提交信息，所以 `show` 改从 `submit-view` 读 `submitted` / `submit_content` / `submit_attachments`，再用 `student/list` 的 `submitTime` 补 `submitted_at`；`submit-view` 失败时命令照常成功并带 `submit_view_error`
- 附件在 `assignmentResource` 字段（不是 `attachmentList`）；`show` 通过 `/blade-source/resource/preview-url` 把 `resourceId` 解析成 `fileucloud.bupt.edu.cn` 下载链接。**CDN 不能带 Blade-Auth，否则 400**

> 本仓库验证时只做了 `--dry-run`，**没有对真实作业执行过提交**。

## 课表与本地缓存

课表查询走**本地缓存**，第一次执行抓取整个学期并写入 `~/.bupt-cli/timetable.json`，之后查询都是读文件（约 0.1 秒，不开浏览器）。

```bash
bupt schedule                 # 本周
bupt schedule --today         # 今天
bupt schedule --tomorrow      # 明天
bupt schedule --date 2026-11-20
bupt schedule --week 12       # 第 12 周
bupt schedule --raw           # 原始课程网格（不分日期）
bupt schedule update          # 刷新缓存
bupt schedule export          # 导出 .ics 到 ~/Downloads
bupt schedule calendar        # 教学周 → 日期对照表
bupt schedule exams           # 考试安排
```

**两种身份都支持**，命令相同，数据源按 `role` 自动选择：

| 身份 | 数据源 | 抓取耗时 |
|---|---|---|
| 研究生（`role: graduate`） | `yjxt.bupt.edu.cn`，经无头 Chrome 过瑞数 | 4–8 秒 |
| 本科生（`role: undergraduate`） | 移动教务 JSON API | 约 1 秒 |

### 缓存何时失效

缓存里存了 `term` 和完整的周→日期表。当**今天不在缓存的任何教学周内**（学期结束、换学期）就视为过期，下次查询会自动重抓。想手动刷新用 `bupt schedule update`，或任意查询加 `--refresh`。

缓存文件损坏或版本不符会被忽略并重建，不会报错。

### 日期是从哪来的

- 研究生：`TermCalender.aspx` 教学日历页，**权威数据**（2026-2027 秋学期第 1 周 = 2026-08-31）
- 本科生：移动教务 `get_curriculum()` 返回的 `SjdDay`，每个教学周都带 `zc`（周次）和 `mxrq`（真实日期）

两条路都**不靠推算**。

> ⚠️ **本科生那条路尚未在真实账号上验证**（写这段代码时没有本科生账号）。数据模型是从 vendored SDK 读出来的，但"一次调用是否返回全部教学周"这点未验证。你在本科账号上跑一次 `bupt schedule update`，然后 `bupt schedule calendar` 检查周数是否合理即可判断。

### 研究生系统的技术细节

研究生系统位于**瑞数（Riversafe）**动态防护之后，纯 HTTP 客户端拿不到数据，所以用无头 Chrome：

- 只依赖系统里的 **Google Chrome**，不需要 Playwright / Selenium / agent-browser——本项目自己写了 CDP 客户端（`src/bupt_cli/cdp.py`，仅标准库）
- 无头模式的关键：默认无头 UA 含 `HeadlessChrome`，CAS 页会**拒绝渲染登录表单**，必须覆盖成桌面 UA
- 上课时间来自课表页的固定作息表：第 1 节 08:00–08:45 … 第 15 节 21:00–21:45

## 错误与退出码

| 退出码 | `error.type` | 含义 |
|---|---|---|
| 0 | — | 成功（`ok:true`） |
| 2 | `usage` | 命令行参数错误 |
| 3 | `auth` | 凭据缺失/无效，或会话失效 |
| 4 | `network` | DNS / TLS / 连接失败 / 超时 |
| 5 | `parse` / `not_found` / `upstream` | 上游返回或解析问题 |
| 6 | —（`verification.status=failed`） | 提交报成功但只读核验未能确认（`verified:false`）；stdout 仍是 `ok:true`，请手动核对，**不要盲目重交** |
| 7 | —（`verification.status=error`） | 提交报成功但核验无法执行（`verified:null`）；请手动核对 |

错误信息里已知的机密值会被脱敏。usage 错误也是 JSON（不是 argparse 的散文），`--help` 仍是普通文本。

## 测试

纯离线，不需要网络和凭据：

```bash
PYTHONPATH=src:.deps ~/.local/share/uv/python/cpython-3.14-macos-aarch64-none/bin/python3.14 -m pytest
```

当前 **190 个测试**，覆盖：时间解析、过滤、附件归一化、`homework list --new` 状态、Cookie 往返、会话文件权限与脱敏、凭据优先级、CLI 错误信封与退出码、电费默认宿舍优先级与 `buildings`、提交后核验（退出码 6/7）、图书馆解析、研究生课表/日历/iCalendar（含跨年周）、CDP WebSocket 协议（假服务端，11 个场景）、自我更新（含 frozen 原位替换）、卸载护栏。

## 已知限制（2026-11 实测）

- `my.bupt.edu.cn` 在公共 DNS 上解析到校内私网地址 `10.3.55.84`。校外 HTTP 超时、HTTPS TLS 被重置 → `notices` / `card` 校外不可用。这是网络范围限制，**不是接口改版**。WebVPN（`webvpn.bupt.edu.cn`）可访问，但用户名密码登录处于维护中（仅支持企业微信扫码），本 CLI 无法代登
- `jwgl.bupt.edu.cn` / `jwxt.bupt.edu.cn` 在校园网外 TLS 重置 → DOM 课表/成绩不可用
- `jwglweixin.bupt.edu.cn` 仅 HTTPS 可达。该后端是**本科**教务，研究生账号不存在，请改用 `bupt schedule`
- 云邮附件在 `assignmentResource` 字段（不是 `attachmentList`），vendored 解析器已打补丁
- 研究生系统有瑞数防护，必须真实浏览器；**无头模式必须覆盖 UA**，且 CAS 表单在 iframe 内，点击要从 iframe 内部派发
- 图书馆电子版覆盖率偏向英文书（英文科技书约八成有，老中文教材基本没有）
- 二进制目前只构建 macOS arm64

## 致谢

本项目的接口与协议细节来自其他人的逆向工作：

### 直接复用代码

| 项目 | 许可 | 说明 |
|---|---|---|
| [Windy-Dance/campus-api](https://github.com/Windy-Dance/campus-api) | MIT | **已 vendored 并适配**到 `src/bupt_cli/_vendor/campus_api/`：CAS 登录、UCloud BladeX token 交换、电费、门户、移动教务客户端。许可证文本见 `src/bupt_cli/_vendor/campus_api/LICENSE`，完整声明见 [NOTICE](NOTICE) |

### 接口调研参考（未拷贝代码）

| 项目 | 许可 | 借鉴了什么 |
|---|---|---|
| [uarix/BUPT-API-Collection](https://github.com/uarix/BUPT-API-Collection) | MIT（声明；仓库无 LICENSE 文件） | 图书馆 OPAC 接口形态 |
| [wzekin/bupt-api](https://github.com/wzekin/bupt-api) | MIT | 早期证据：CAS 表单在隐藏字段后明文提交密码 |
| [Yokumii/bupt-api-collected](https://github.com/Yokumii/bupt-api-collected) | Apache-2.0 | 移动教务是 AES 而非 CAS；考试成绩接口名 |
| ZeroCore-Zero/BUPT_APIS、YouXam/ucloud | MIT | 端点线索 |

研究生系统接入（`bupt schedule`）、图书馆客户端、iCalendar 导出、无头 CDP 驱动与瑞数应对是为本项目写的，均在 2026-11 实测验证。

如果要从本仓库拷贝代码，请同时遵守上游项目的许可。

## 许可

MIT，见 [LICENSE](LICENSE) 与 [NOTICE](NOTICE)。vendored 的 campus-api 同样是 MIT。
