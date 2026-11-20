# winget manifest（Tututuhehehe.Bupt，portable 包）

在真实 Windows 机器上执行通过后再提交到 microsoft/winget-pkgs。
提交路径：`manifests/t/tututuhehehe/Bupt/<version>/`，两个 YAML 一起提一个 PR。

## manifests/t/tututuhehehe/Bupt/tututuhehehe.Bupt.yaml

```yaml
# Winget Package Id: Tututuhehehe.Bupt
PackageIdentifier: Tututuhehehe.Bupt
PackageVersion: 0.6.0
PackageName: bupt
Publisher: tututuhehehe
License: MIT
LicenseUrl: https://github.com/tututuhehehe/bupt-cli/blob/main/LICENSE
ShortDescription: Command-line client for BUPT campus services (unofficial)
PackageUrl: https://github.com/tututuhehehe/bupt-cli
Moniker: bupt
Tags:
  - cli
  - bupt
  - university
ManifestType: singleton
ManifestVersion: 1.6.0
```

## manifests/t/tututuhehehe/Bupt/tututuhehehe.Bupt.installer.yaml

```yaml
PackageIdentifier: Tututuhehehe.Bupt
PackageVersion: 0.6.0
InstallerLocale: en-US
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
  - RelativeFilePath: bupt-windows-x64/bupt.exe
    PortableCommandAlias: bupt
InstallerUrl: https://github.com/tututuhehehe/bupt-cli/releases/download/v0.6.0/bupt-windows-x64.zip
InstallerSha256: 7ebb7e60837c238153b5a0c640570af5ef9902636a2f0b91c0e42eb178b6d9c6
InstallModes:
  - silent
UpgradeBehavior: uninstallPrevious
Commands:
  - bupt
ManifestType: installer
ManifestVersion: 1.6.0
```
