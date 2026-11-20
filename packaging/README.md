# 分发指南（Homebrew / Scoop / winget）

发布物是单文件二进制 zip（无运行时依赖，只调用系统 `curl`）：

- macOS Apple Silicon：`bupt-macos-arm64.zip`（`./build-rust.sh` 产出，含 SHA-256）
- Windows x64：`bupt-windows-x64.zip`（Windows 实机验收后产出）

## 发布流程（每个版本）

1. `./build-rust.sh` → `dist/bupt-macos-arm64.zip` + `.sha256`
2. GitHub 上打 tag `vX.Y.Z`（与 Cargo.toml 一致），把 zip 与 `.sha256` 附到 Release
3. 更新下面对应的清单（Formula / Scoop / winget）里的版本号与 SHA-256

## Homebrew（macOS，自建 tap）

1. GitHub 建空仓库 `tututuhehehe/homebrew-tap`（名字必须是 `homebrew-` 前缀）
2. 把本仓库 `packaging/Formula/bupt.rb` 放进那个仓库的 `Formula/bupt.rb` 并提交
3. 用户一键安装：

```bash
brew tap tututuhehehe/tap https://github.com/tututuhehehe/homebrew-tap
brew install bupt            # 或 brew install tututuhehehe/tap/bupt
```

要求与注意：

- 下载地址必须是**带版本号的 Release 直链**；Homebrew 拒绝 `releases/latest` 重定向
- sha256 必须与附件一致（`brew audit --strict` 会校验）
- 当前只提供 Apple Silicon；Intel Mac 需要另出 x86_64 构建，或保持
  `depends_on arch: :arm64`（现在的默认）
- 升级新版本 = 改 Formula 的 url + sha256，用户 `brew upgrade bupt`

## Scoop（Windows，自建 bucket，最快）

1. GitHub 建仓库 `tututuhehehe/scoop-bucket`
2. 放 `bucket/bupt.json`（模板如下），`url` 指向 Release 的 zip，`hash` 填 SHA-256
3. 用户安装：

```powershell
scoop bucket add tututuhehehe https://github.com/tututuhehehe/scoop-bucket
scoop install bupt
```

清单已放在本仓库 `packaging/scoop/bucket/bupt.json`（交叉编译完成后把 `hash` 换成实测 SHA-256）。

当前状态：`bupt-windows-x64.zip` 是**交叉编译产物，尚未在真实 Windows 上执行**。在你那台 Win 电脑上解包跑 `bupt.exe --version / help / status / login --role graduate` 通过后，再把 bucket 公开宣传 / 提交 winget。

## winget（Windows，覆盖最广，一次性 PR）

向 `microsoft/winget-pkgs` 提交 portable 清单（`manifests/t/tututuhehehe/bupt/…`），
指向同一 Release zip；用户 `winget install bupt`（无需先加源）。

- 需要字段：PackageIdentifier（如 `Tututuhehehe.Bupt`）、版本、InstallerUrl、
  InstallerSha256、PortableCommandAlias `bupt`
- 审核是一次性的，之后每个新版本提一个更新 PR
- 前提：`bupt.exe` 在 Windows 10/11 上实测通过（二进制调用系统 `curl.exe`，无需管理员权限）

## 本机安装约定

仓库根目录的 `./bupt` 是本机安装位置（`~/.zshrc` 把 `moon_cli_or_tool/*` 加进
PATH），不进 git；换版本就重跑 `./build-rust.sh` 并解包覆盖。
