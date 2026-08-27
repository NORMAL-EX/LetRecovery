# Defender 引擎移除的句柄安全边界

本文补充 `DEFENDER_ENGINE_REMOVAL.md` 的文件系统实现约束。白名单范围、8 个 Defender Antivirus 引擎服务以及必须保留的 Windows Security、Firewall、SmartScreen、UAC、VBS 和 Defender for Endpoint 边界均保持不变。

## 对象固定

调用方必须先把目标 `SYSTEM`、`SOFTWARE` 配置单元通过 `RegLoadKeyW` 加载到本次固定别名；共享移除边界只保留其普通非重解析祖先目录的身份 pins，不得在 hive 已加载后再次用 `CreateFileW` 打开 hive 文件。微软的文件共享契约要求后来的访问与既有句柄共享模式相容，重复打开已加载 hive 在受支持系统上可合法返回 sharing/access error，而且该文件句柄既不能证明注册表别名来自该文件，也不保护后续白名单文件写入。每个 Defender 白名单根和递归子项仍必须先以 `FILE_FLAG_OPEN_REPARSE_POINT` 打开；对象句柄拒绝 delete sharing，并记录卷序列号、文件 ID、对象类型与硬链接计数。

以下状态一律失败关闭：

- 目标、祖先或子项是 reparse point、junction 或符号链接；
- 文件具有多个硬链接，或文件系统没有返回稳定的非零文件 ID；
- 对象既不是普通文件也不是普通目录；
- 路径重新打开后的 volume/file ID 与保留句柄不一致；
- 枚举期间出现身份替换、未枚举并发子项或删除后同名抢占。

## 同一句柄上的变更

owner 和 DACL 修改使用 `SetSecurityInfo` 作用于已验证的同一对象句柄；只读属性通过 `SetFileInformationByHandle(FileBasicInfo)` 修改；最终删除通过该句柄的 `SetFileInformationByHandle(FileDispositionInfo)` 完成。不得再使用 `SetNamedSecurityInfoW`、`set_permissions`、`remove_file` 或 `remove_dir` 按路径重新选取对象。

路径只用于目录枚举。枚举前后及每个子项处理后都必须重新打开并比较同一 volume/file ID。现代文件系统即使允许持有句柄时重命名，也只能触发身份复核失败，不能把替换对象带入递归删除。并发新增的未枚举子项会使目录删除以非空失败，未知对象保持不动。

## 测试边界

自动测试只在普通临时目录中验证：离线目标验证器不会重复持有 hive 文件句柄，以及白名单对象的重命名/替换竞态、重解析目录和多硬链接拒绝。测试不得加载离线注册表、调用 DISM、修改宿主机 Defender，也不得执行任何真实系统移除。
