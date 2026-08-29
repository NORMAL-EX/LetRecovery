# 全盘重装与双系统交接经验

本文记录 LetRecovery 正常系统端、私有启动 WIM 与 WinPE 之间的磁盘交接边界。它描述的是代码约束，不是实体磁盘已经验证成功的声明；在可丢弃虚拟机日志跨过历史故障点前，只能认为代码和非破坏性测试完成。

## 历史误阻断

曾出现过这样的失败：Windows/VDS 已完成缩卷并返回一个合法的当前 extent，随后共享建分区代码又用固定 `% 1 MiB` 检查显式 offset，最终报错：

```text
validate partition: explicit partition offset must be 1 MiB aligned
```

同类错误还包括把 `IVdsCreatePartitionEx::CreatePartitionEx` 的 `ulAlign` 错写成 `1`，
误以为它表示“关闭对齐”。`ulAlign` 的单位是字节，`1` 会在普通 512/4096 字节扇区磁盘上
触发 `VDS_E_ALIGN_NOT_SECTOR_SIZE_MULTIPLE (0x80042554)`。不指定额外对齐要求时必须传 `0`，
让 VDS 使用磁盘/provider 的合法约束，再用创建后的实际 extent 回读收束。

错误原因不是磁盘布局损坏，而是把常见布局偏好误当成 Windows 合法性契约。VDS 返回的 free extent 才是当前会话事实；既有 extent 也可能满足真实扇区约束但不是整 MiB。

回归规则：

- 显式 provider offset 原值传递，不做 `align_up`、`align_down`、截断或取整。
- 1 MiB 创建偏好只体现在请求 offset；`ulAlign=0` 让 VDS 使用当前设备/provider 的合法 alignment。禁止用 `1` 充当布尔 false，也不再额外传固定 1 MiB 对齐参数。
- 没有显式 offset 时可优先 1 MiB 起点；偏好位置放不下时必须回退到 provider 原始 extent。
- 只检查非空、checked overflow、完全包含于 provider 空闲范围、与受保护范围不重叠，以及操作后的真实布局回读。
- 新分区实际 offset/length 可以与请求略有差异，只要仍在选定 provider extent 内且容量不小于最低需求。
- `CreatePartitionEx` 异步操作一旦启动，`Wait`、`Refresh`、首次布局回读、格式化、盘符分配或最终回读失败都不能证明没有产生写盘。共享存储层必须用操作前与当前 canonical 布局收束：未变化时返回原错；只有当前布局证明唯一新增 extent 的角色、最低容量和本次选定授权 envelope 包含关系全部满足时，才按该实际 extent 执行 checked delete；额外变化、歧义或回读失败必须报告 partial state 并保留现场，调用端不得按请求 offset 或路径猜测清理。
- 回滚扩容同样以操作前相邻 provider free extent 和操作后实际卷范围为准；实际增加量可因 provider 合法取整略大于请求，但必须不少于最低需求且完全留在该已授权相邻 extent 内。
- 原始块 I/O 的对齐只能读取当前物理盘 `StorageAccessAlignmentProperty`，不能用固定 1 MiB、4 KiB 或 512 B 推断。`CreatePartitionEx.ulAlign` 则是独立的可选额外对齐请求，不是 logical/physical sector 字段；普通与 caller-authorized 创建都传 `0` 让 VDS/provider 决定，仅用 `BytesPerLogicalSector` 把 `ullSize` 收束为完整逻辑扇区字节数。

2026-08-18 的第六份可丢弃虚拟机现场补足了上一条的另一半：同盘 canonical `IOCTL_DISK_GET_DRIVE_LAYOUT_EX` 已明确为 GPT，但刷新后的 `VDS_DISK_PROP.PartitionStyle` 仍不是 GPT，旧分支因此构造了错误的 `CREATE_PARTITION_PARAMETERS` union；日志中的 `alignment=0` 与连续两次 `E_INVALIDARG` 不能单独证明 provider-default alignment 错误。磁盘样式决定 GPT/MBR 参数 union、预期 token、初始化完成态和 MBR active 参数；这些承重输入必须全部来自同一份紧邻操作且再次回读验证的 canonical IOCTL layout。`VDS_DISK_PROP.PartitionStyle` 只允许记录矛盾诊断，不得决定这些输入。共享层仍可使用当前 VDS 对象执行 COM 操作，但不能让该对象的缓存样式覆盖物理盘分区表事实。

2026-08-18 的下一份同机现场最终解释了上述“同一个 VDS 对象同时报告错误样式和 2048 字节扇区”的组合：虚拟机同时存在物理磁盘 0 与光驱 0，旧 VDS locator 只消费 `STORAGE_DEVICE_NUMBER.DeviceNumber`，丢弃了同一结构中的 `DeviceType`，因此把 `FILE_DEVICE_CD_ROM/FILE_DEVICE_DVD` 的设备 0 误绑定成 `FILE_DEVICE_DISK` 0。微软对 `VDS_DISK_PROP.dwDeviceType` 明确列出 CD-ROM、硬盘和 DVD，并把 `STORAGE_DEVICE_NUMBER` 定义为设备类型、设备号和分区号的联合结果；设备号只有在同一设备类型内才有意义。所有 VDS 物理盘候选必须先要求 `dwDeviceType == FILE_DEVICE_DISK`，打开 locator 后还必须要求 IOCTL 回读的 `DeviceType == FILE_DEVICE_DISK`，然后才比较 `DeviceNumber`。光驱 0 必须作为无关 VDS 对象静默淘汰，不能因数字相同获得物理盘 0 的写盘权限；这条规则同时适用于 legacy 与 extended device-number IOCTL，并由“CD/DVD 0 不得别名到 disk 0”的回归测试固定。

过滤光驱别名后的下一次同机日志证明了另一个独立错误：当前 VDS 对象已经唯一回绑真实 `FILE_DEVICE_DISK` 0，canonical GPT 参数也正确，但 `CreatePartitionEx(..., ulAlign=512, ...)` 仍明确返回 `VDS_E_ALIGN_NOT_SECTOR_SIZE_MULTIPLE (0x80042554)`。微软只把 `ulAlign` 定义为可选的“alignment size in bytes”，`0` 表示由 server 根据磁盘决定；它没有把 `BytesPerLogicalSector` 或 `BytesPerPhysicalSector` 定义为该参数的必传值。此前“logical sector 必须作为 ulAlign”的结论由此被实机反证，必须连同测试和文档删除。修正后 offset 原值传给 VDS、`ulAlign=0`，desired/minimum size 才按权威 logical sector 转为完整扇区，操作后的实际 extent 仍由 canonical layout delta 和 caller envelope 收束。

随后同一台 80 GiB、512e 虚拟机证明，仅把 `ulAlign` 改为 `0` 仍不能假设 caller 的 desired tail 会完整保留：正常端缩出 10,072,621,056 字节，`CreatePartitionEx` 将起点从 75,826,707,968 调到 75,826,724,864，实际分区因此为 10,071,572,480 字节，比 10,072,182,906 字节功能最低值少 610,426 字节。创建本身成功，旧代码却把这一合法 provider 调整升级为失败，而新分区已经占住 tail，扩回自然又跨越下一分区。微软 MS-VDS Appendix B 明确说明 `ulAlign=0` 时 server 按磁盘大小读取 `HKLM\System\CurrentControlSet\Services\vds\Alignment` 的 `LessThan4GB`、`Between4_8GB`、`Between8_32GB` 或 `GreaterThan32GB`，默认分别为 64 KiB/1 MiB，并允许管理员覆盖。正常端必须在仍可逆的 Shrink 规划阶段读取当前 canonical 磁盘容量和对应 DWORD，以“目标预算之前的起点余数”精确增加拓扑回收字节，使新 tail 起点已经落在 server 将采用的边界；该额外字节只是不写入数据的当前布局残差，不能重复叠加到 payload 加固定 2 GiB 的功能预算。Shrink 后仍必须把实际 tail offset 原样传给 `CreatePartitionEx`，不得再对 provider extent 对齐、取整或要求逐字节等于请求；注册表值零、读取失败、操作后最小容量不满足或 canonical delta 异常才按既有可逆/partial-state规则处理。回归测试必须覆盖非整 MiB 源末端和非 MiB 管理员覆盖值。

2026-08-12 的可丢弃虚拟机又证明了同一类前提错误：全盘 `Clean` 已完成、VDS 已创建唯一一个 GPT EFI 分区，随后代码却把传给 `CreatePartitionEx` 的 provider-default 建议起点同时当成创建结果不可向下越过的授权下界，因而在不可逆边界后报 `created extent is outside the selected provider range`。微软契约明确说明 `ulAlign=0` 时 provider 可以把请求 offset 向上或向下取整。2026-08-13 的实际日志进一步证明，`offset + size` 也只是 desired geometry：VDS 在当前 raw/provider free extent 内创建了完整 300 MiB ESP，但旧代码把 desired end 当成 hard end，因实际起点变化而误报越界。修正后的四个量必须分开：传给 provider 的 desired offset/size、操作前共享层自行发现的 raw/provider 当前 free extent 或调用方已经从 canonical 事实推导出的精确 envelope、分区角色的功能最低容量、以及操作后 canonical 布局中的 observed extent。没有 caller envelope 时，raw/provider 交集是共享层的创建授权；已经存在精确 caller envelope 时，不得再用可能滞后的 VDS 库存缩窄它。desired end 也不能再次缩窄调用方已经明确给出的合法 envelope。`QueryFreeExtents(0)` 只参与无 caller envelope 时的创建区间发现，不能重新发明单向取整、逐字节相等或二次授权契约。

全盘启动基础设施的最低值也必须与兼容目标一致，而不是统一降成“非空即可”：ESP 按当前磁盘权威 logical-sector 查询区分 512/512e 的 200 MiB 与 4Kn 的 300 MiB；查询失败或返回未知/矛盾值时在首次创建前停止，禁止猜 512 B/4 KiB。为兼容 Windows 7，GPT MSR 使用 128 MiB；BIOS System Reserved 至少保留 BitLocker 所需的 350 MiB 功能下限。布局请求可以更大，但 provider 实际结果只有同时满足角色最低值、当前授权范围和后续承重分区预算才可接受。

同次事故还暴露出把普通 GPT 新分区的随机 `partitionId` 和 attributes 混称为“分区角色”的错误。普通创建的角色只由 GPT partition type 决定；唯一新增对象由 canonical 布局 delta 证明。只有离线块搬移重建明确要求保留 GPT metadata 时才独立精确核对 partition GUID、attributes 和 name。任何失败回滚都必须保存首次权威回读到的实际完整 token 与 extent，并按该实际对象精确重绑定后删除，禁止继续用创建前随机 token 猜测清理。回归测试必须同时覆盖自动 offset 合法向下取整、显式 offset/envelope 越界拒绝、普通 GPT 同 type 不同 provider ID 接受、保留 metadata 失败拒绝，以及异步错误后按实际 token 回滚。

另一类真实误阻断发生在新分区格式化后分配盘符：`IVdsVolumeMF::AddAccessPath`
返回 `S_FALSE (0x00000001)` 时，微软的接口契约表示访问路径已经添加成功，只是辅助的 GPT
no-drive-letter 属性或默认共享更新可能未完成。它不是 Win32 `ERROR_INVALID_FUNCTION`，也不能被
通用 HRESULT 格式化后当成失败并回滚刚创建的分区。共享边界只在该 API 上接受 `S_OK` 与
`S_FALSE`，随后有界等待盘符可见，并从新盘符句柄通过
`IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` 回读本次 provider 实际创建的完整 extent；换绑或始终
不可读才失败。`AssignDriveLetter`、`DeleteAccessPath` 等其它 VDS 调用仍按它们各自的官方成功码
处理，禁止把 warning 策略全局放宽。

2026-08-13 的 Cloud-PE 虚拟机在镜像应用成功、并已从磁盘 0 offset 1048576 找到 GPT ESP 后，仍在修复引导阶段报告“没有可用的 ESP”。根因不是 ESP 缺失，而是 checked 盘符分配在进入隐藏分区路径前先枚举全机普通 `IVdsVolume`；无关的 WinPE `X:` RAM disk 无法提供物理磁盘 extent，单个对象错误被错误升级为目标 ESP 失败。微软契约明确把 ESP 归入 `IVdsAdvancedDisk` 管理的 hidden partition：canonical 布局已经证明目标为 GPT ESP 后，必须直接以当前磁盘和字节 offset 调用 `AssignDriveLetter/GetDriveLetter/DeleteDriveLetter`，不得要求无关普通 volume 全部可读。普通基础数据分区仍必须唯一绑定精确 extent 的 `IVdsVolume`，找不到时失败，禁止为了绕过噪声而错误降级到 AdvancedDisk。

`AssignDriveLetter` 的 `S_OK` 只证明调用已提交；后续 `Refresh`、盘符 extent 或布局回读失败不能假设盘符不存在。共享边界必须保留 AdvancedDisk 对象并执行 post-commit reconciliation：只有 `GetDriveLetter` 与当前完整 extent 唯一证明该字母仍属于本次目标时才按同一 disk+offset 对称删除；盘符换绑、证据矛盾或不可确认时禁止删除并报告 partial state，删除或删除后验证失败还要同时保留原始失败与 cleanup 结果。临时盘符选择使用 `GetLogicalDrives` 时，返回零是 API 查询失败而不是“所有盘符都空闲”或“盘符耗尽”，必须保留该错误语义。

2026-08-13 的受支持 Windows 11 24H2 虚拟机已经跨过镜像应用与 UEFI 引导，却在首次启动 `specialize` 阶段拒绝 `C:\Windows\Panther\unattend.xml`。只读提取的 Windows Setup 日志把 `0x80220005 / WCM_E_INVALIDVALUE` 精确定位到 `Microsoft-Windows-Deployment/RunSynchronousCommand[Order=4]/Path`：Reserved Storage 启动命令解码后为 300 字符，而微软对该字段的上限是 259；同仓 SecHealthUI 为 296，自定义 RID-500 的整段 `EncodedCommand` 更超过 1300。XML well-formed、UTF-8 编码和“包含预期字符串”测试都不能证明 SMI 字段值合法。两端所有内置 specialize 命令必须经共享边界按解码后 UTF-16 长度、Order 1–500、非空和 XML 字符验证；固定脚本用短启动器，RID-500 名称按 PowerShell 单引号字面量规则转义并在 20 UTF-16 上限下保持 Path 小于 259。FirstLogon 的脚本调用与目录清理必须合并为单个、低于 1024 字符的 CommandLine，不能让独立清理项与脚本形成竞态。复杂的目标-image schema 仍应由匹配实际 WIM/catalog 的 Windows SIM 验证，运行时不得用自制通用 schema 猜测器扩大误报。

2026-08-17 的 Cloud-PE 虚拟机又暴露了“读取对象不是当前事实”的同类错误：物理盘的 `IOCTL_DISK_GET_DRIVE_LAYOUT_EX` 和 MBR 签名读取均明确显示 GPT，但正常端库存从一个未刷新 VDS disk object 的 `VDS_DISK_PROP.PartitionStyle` 得到 MBR，Auto 因而错误进入 BIOS BCDBoot、Bootsect 和 `IVdsAdvancedDisk::ChangeAttributes(bootIndicator)`，最后以 `VDS_E_OBJECT_NOT_FOUND (0x80042405)` 失败。微软定义 `DRIVE_LAYOUT_INFORMATION_EX.PartitionStyle` 为当前磁盘分区样式，并定义 `bootIndicator` 只属于 MBR 参数；因此磁盘样式和 Auto BIOS/UEFI 决策必须统一使用当前物理盘 IOCTL 回读，Legacy 路径若回读不是 MBR 必须在写 BCDBoot/Bootsect 前停止。VDS 的 `Reenumerate`/`Refresh` 仍用于更新操作对象，但不能让另一个缓存对象覆盖同一边界的权威布局事实。

同日重建虚拟机后的下一次日志确认 Auto 已正确选择 UEFI，却在 ESP 盘符分配前报告同一 `PhysicalDrive0` 有两个可读 VDS disk object。微软要求完整查看主机磁盘时跨所有 software provider 查询；provider/pack/object 是管理层级，不是新的物理身份。只要每个候选的 `pwszName/pwszDevicePath` 都通过 `IOCTL_STORAGE_GET_DEVICE_NUMBER` 回绑同一当前物理盘，并且该候选的 `IVdsAdvancedDisk::GetPartitionProperties` 在 canonical offset 返回 GPT ESP，多个候选就是同一 `disk+offset+role` 的 VDS alias，不能误报为两个目标。选择一个可操作 alias 前后仍必须用 canonical IOCTL layout 复核，候选无法识别精确 ESP 时只淘汰该候选；零个可用 alias 才失败。禁止为去重叠加容量、VDS GUID 或枚举顺序等新的跨阶段身份门槛。

## 正常端职责

### 全盘重装

正常端只使用 SetupAPI `GUID_DEVINTERFACE_DISK` 枚举当前存在的磁盘接口，并把 UI 实际展示、用户明确确认的内部磁盘写入强类型计划。计划必须恰有一个 Windows 目标；其它选中磁盘只创建数据卷。

Windows 最低容量来自所选镜像分卷的 `TOTALBYTES - HARDLINKBYTES + 2 GiB`；元数据缺失或 GHO/GHS 使用 80 GiB 回退。剩余空间不足以形成有用数据卷时，Windows 使用全部可用空间，不得为了数据卷阻断。

“Windows 使用全部可用空间”只是当前布局结果，不能把交接计划中的镜像最低容量改写成 `0`。同盘暂存可能在正常端确认计划后把 PE 可用末端前移；PE 必须仍用原镜像最低值重新规划并在不足时于首次写盘前停止。远程服务器不支持 Range、尚未取得本地分卷元数据时，80 GiB 只能作为未知镜像的显示回退，不能在完整下载前阻断下载；程序必须先下载并从本地选择第一个可安装分卷，再按实际 `TOTALBYTES - HARDLINKBYTES + 2 GiB` 生成计划和确认框。

每个选中磁盘使用独立 CNG 随机 locator。正常端只在该磁盘当前已有的可写基础数据卷根发布固定名 locator；locator 之外的磁盘号、盘符、GUID、容量和历史布局只用于当前 UI 与日志，不参与 WinPE 跨重启定位。同盘暂存 extent 作为既有范围原值交接，非整 MiB 不是错误。

### 双系统

缩卷和目标卷创建全部在完整 Windows 中完成。正常端：

[`IVdsVolumeShrink::QueryMaxReclaimableBytes`](https://learn.microsoft.com/en-us/windows/win32/api/vds/nf-vds-ivdsvolumeshrink-querymaxreclaimablebytes)
只能提供界面和日志诊断；微软明确把它称为 Shrink 的估计值，并说明它可能返回比实际可回收量更多的字节。查询失败、估计偏大或估计偏小
都不能单独证明真实 Shrink 会成功或失败。权威边界是带明确 desired/minimum 的真实 Shrink
返回结果，以及紧随其后的当前卷 extent 回读。该 API 从 Windows Vista 起提供，因此正常端
Windows 7 最低版本不需要命令行回退。

1. 读取用户选择的源卷当前 extent；`QueryMaxReclaimableBytes` 只作为界面诊断估计，查询失败、估计值偏小或偏大都不得代替真实 Shrink 结果。
2. 请求所需最低容量并执行一次真实 VDS Shrink。
3. 立即回读源卷实际长度；只要求它确实缩小且实际回收量不少于最低需求。异步输出中的 `ullReclaimedBytes` 与当前 extent 的差异只记录诊断，不要求逐字节相等。
   如果真实 Shrink 已提交、但随后的 VDS Refresh 或 IOCTL 回读失败，共享调用会返回错误却不能证明卷未变化。正常端仍处于可逆阶段时必须再读取同一当前卷：只有观察到同磁盘、同起点的实际尾部缩小时才按观察量扩回；未变化时不操作，无法读取、换绑或范围异常时不得按请求值、QueryMax 估计值或异步输出盲目扩容，并必须把原错误和恢复结果一起报告。
4. 以同盘同起点的 Shrink 前后卷 extent 推导出的精确 reclaimed tail 作为 caller authorization envelope；不要在已确认 Shrink 之后再做 provider free-extent 门禁。checked create 只用紧邻真实 `CreatePartitionEx` 的 canonical 布局证明该 envelope 仍未与现有分区重叠，再以操作后唯一 canonical delta 验证实际结果。`Refresh` 和 `IOCTL_DISK_UPDATE_PROPERTIES` 可以帮助缓存收敛，但都不能把仍滞后的 `QueryExtents`/`QueryFreeExtents` 结果升级成新的授权事实。
5. 回读实际创建范围，把这些实际值写入同一 `DualBootPlan` 后才生成私有 WIM 交接。

只有单个 C: 且没有其它足够数据卷时，正常端先计算实际将写入文件的精确预算加 2 GiB，
再把 Windows 目标卷和数据/暂存卷合并到同一个 move-only 事务中，只执行一次 Shrink。
后续配置阶段复用该事务，不得先因缺少数据卷失败，也不得二次缩卷。首次破坏性写入前任一
阶段失败时，释放本次锁定的输入，删除本次预创建卷并按真实回收量扩回源卷。

重试先识别强类型计划中已经预创建的精确 target/data extent；若完整存在则复用，若只完成一半则拒绝第二次缩卷并进入本次事务回滚。WinPE 永远不再 Shrink，也不重新创建双系统目标。

## WinPE 职责

WinPE 先从当前 `X:` 私有启动 WIM 读取 LRHC1/config/LRHM3。data 与 install target 分别使用不同的 256-bit locator；磁盘上同名但内容不同、损坏、不可读或重解析 marker 都是环境噪声，必须静默忽略。只有零个精确匹配或多个不同卷精确匹配才失败。候选集必须来自 `FindFirstVolumeW`/`FindNextVolumeW` 的当前 volume GUID namespace，不能只轮询 `A:`…`Z:`；卷 GUID 路径只是本次启动的访问路径，不进入认证配置或身份指纹。

2026-08-24 的 `before_target_write` 可丢弃 Hyper-V 故障注入证明，VDS provider 的实际 Shrink 量可以合法地比随后创建的暂存分区大：该轮多回收了 `1,031,680` 字节，并把它留在暂存分区之后。若跨重启清理只按暂存分区长度扩回，旧系统仍能启动，但源卷会永久少掉这段尾隙，不能称为完整回退。LRHM3 因此把正常端回读到的 exact pre-shrink source length 与 post-shrink canonical source/temporary pair 一起纳入 HMAC 认证；PE 先确认临时分区前后空隙仍是当前 provider free extent，只删除受认证的临时 extent，再按 `pre-shrink length - post-shrink length` 调用真实 Extend，并要求最终同盘、同起点、长度逐字节等于缩前值。该长度是本次恢复事务的最小授权，不参与普通目标跨启动定位，也不得被整 MiB、固定扇区或“应等于临时分区大小”的经验判断替代。

持久 PE 的 LRPE4 journal 仅用于删除本会话的有界 BCD 对象和私有 WIM/SDI 文件。运行中私有 WIM 已提供精确 `SessionId + purpose + capsule SHA-256`，因此 journal 中的历史磁盘布局/GUID/extent 只保留兼容解析和诊断，不再参与跨启动匹配。损坏、异内容或其它会话记录静默忽略；零个或多个完整三元组匹配仍停止清理。唯一匹配的 journal 必须以普通非重解析文件句柄拒绝 write/delete sharing，从解析、BCD/payload 删除一直持有到最后一次未变回读；它只认证该精确记录，不把旧几何重新升格为指纹。

全盘重装只处理计划中 locator 已唯一命中的磁盘。无同盘暂存时才整盘 clean；同盘暂存由本次 data locator 唯一匹配卷的当前 extent 重新绑定，正常端历史 offset/length 只记诊断；当前分区表确认它是唯一 basic-data、非空、在盘内且不与当前目标重叠后，才保留该实际 extent并只删除同盘其它旧分区。新布局把 1 MiB 当无显式 provider offset 时的偏好，实际创建结果以 VDS 和操作后读取为准。全盘首次拓扑写入前只关闭已验证 locator 的持有句柄，不把逐个删除 marker 设成新门禁：旧分区马上会被 checked clean/delete 移除，提前逐文件删除没有额外安全收益，却会在中途失败时破坏任务重试材料。

同盘暂存只在镜像、驱动、无人值守和引导全部成功后尝试回收。最终新卷与暂存之间允许 provider 留下非整 MiB 的合法空闲尾隙；回收前确认 recipient、空闲尾隙、staging 三段当前连续且没有其它分区，再删除 staging 并把尾隙与 staging 一并扩入 recipient。回收失败只能报告“安装完成但暂存清理失败”并禁止自动重启，不能把已可启动的新系统改判为安装失败。

2026-08-14 的全盘同盘暂存日志暴露了一个不可逆边界后的高误报：交接认证、manifest 哈希、镜像完整性和 staging 当前 extent 均已通过，但布局执行器在每次新建分区后立刻要求该分区具有卷访问路径。GPT ESP 和 MSR 按契约本来就没有盘符，于是流程在第一个隐藏基础分区上错误报告“暂存之前最后一个卷没有访问路径”，尚未继续创建 Windows 普通卷就停止。接收卷选择不得逐分区失败；必须静默跳过 ESP、MSR、Recovery 等无盘符基础分区，在完整创建序列中只比较暂存之前当前已挂载的普通卷，并选择结束位置最靠近暂存的一项。只有布局完成后仍没有任何合格普通卷才失败。回归测试必须同时覆盖 ESP/MSR 在前、Windows 普通卷在后、后续隐藏 Recovery 不得覆盖已选接收卷，以及多个普通卷时选择最靠近暂存者；offset 和 length 必须包含扇区合法但非整 MiB 的值。

2026-08-16 的同盘暂存清理又出现一项确定的路径契约误用：安装、引导和无人值守均已完成，但物理磁盘枚举尝试从 `VDS_DISK_PROP.pwszName/pwszDevicePath` 文本中截取 `PhysicalDriveN`，遇到合法的 PnP 符号路径后误报“device path do not contain a physical disk number”，从而保留暂存并抑制重启。微软对 `SetupDiGetDeviceInterfaceDetailW` 返回的设备路径明确要求将其作为不透明的 `CreateFileW` locator，禁止解析 symbolic name；`VDS_DISK_PROP` 也只把 `pwszName` 定义为可打开的名称、把 `pwszDevicePath` 定义为 PnP 路径，并未承诺任何数字后缀。当前物理磁盘候选集必须由 `SetupDiGetClassDevsW(GUID_DEVINTERFACE_DISK, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)` 枚举，并对打开的实际路径调用 `IOCTL_STORAGE_GET_DEVICE_NUMBER` 获取当前磁盘号；该 IOCTL 最低 Windows XP，覆盖 Windows 7。隐藏分区必须回绑 VDS disk object 时，同样只允许原样打开 VDS locator 后调用该 IOCTL，严禁恢复字符串解析。`VDS_S_PROPERTIES_INCOMPLETE` 只要 locator 已返回且句柄 IOCTL 成功，就不得因无关 health/status 属性缺失误阻断。

2026-08-18 的两轮可丢弃 Hyper-V 日志和只读 VHD/AVHDX 布局回读证明了两层相同的高误报。第一轮中 C: 的真实 VDS Shrink 已成功提交并回读为同盘同起点尾缩，VHD 尾部实际存在 9,607 MiB 未分配空间，但正常端随后独立调用 `QueryFreeExtents` 的预检读取到尚未收敛的 provider 缓存，误报“没有可用 free extent”。移除外层预检后，第二轮仍在共享 checked create 内因 `QueryExtents`/`QueryFreeExtents` 的旧库存无交集而停止；回滚 Extend 也被同一旧 `QueryExtents` 视图拦住。这证明 `Refresh` 只能刷新 VDS 对磁盘驱动已知布局的缓存，不能保证驱动或所有 provider 库存已经在这一刻收敛。Shrink 后只允许用当前卷 canonical extent 推导精确 reclaimed tail；该 tail 是 caller 授权范围，不是伪造的 provider 几何。`create_partition_checked_in_envelope` 必须在真实调用边界复核 canonical 布局仍无重叠，然后直接调用 `CreatePartitionEx` 并核对唯一 canonical layout delta；`IOCTL_DISK_UPDATE_PROPERTIES` 只能 best-effort 促使缓存收敛，失败为 warning，禁止把任何 VDS 库存查询重新叠加成阻断。可逆 Extend 回滚则以当前源 extent 与 canonical 下一分区起点或磁盘末端确定相邻上界，调用真实 `IVdsVolume::Extend` 后回读实际增长量，不再要求 `QueryExtents` 先声明空闲。回归测试必须证明扇区合法但非整 MiB 的 tail 原值贯穿 authorization envelope，并且创建与回滚都不依赖滞后的 VDS inventory。

同日第三至第五轮日志当时被错误解释为 provider-default alignment 问题，后续现场已推翻该结论：`alignment=0` 的两次 `E_INVALIDARG` 发生在错误 VDS 对象/错误 partition-style union 尚未排除时；所谓 VDS 2048 字节“硬盘扇区”实际来自设备号同为 0 的光驱。不能从这些混杂变量推出 `BytesPerLogicalSector` 应传入 `ulAlign`。有效结论只保留三项：caller-authorized offset 原值传递；desired/minimum `ullSize` 使用当前真实 logical sector 形成完整扇区；`ulAlign=0` 让 VDS/provider 执行其受支持的对齐规则。创建后的唯一 canonical delta 必须位于授权 envelope 且不少于 minimum，任何进一步判断都不能重新使用已被反证的 2048 字节样本。

2026-08-18 的发布版完整 PE 日志还证明了镜像目录外形不能替代已认证的镜像元数据：该次 ESD 已成功释放并明确报告 Windows build 16299，驱动和 UEFI `bcdboot` 也已成功，真正失败点是随后加载离线 `Windows\System32\config\SOFTWARE` 时对路径执行了不必要的 canonicalize。精简 Vista+ 镜像和部分 GHO 可以合法缺少 `Windows\Boot`，因此正常端和 PE 端都不得用该目录是否存在重新推断 XP/2003，也不得在真实引导修复前用这一库存观察阻断。系统类型只来自 DISM/GHO 已验证元数据和受认证配置；离线 hive 以已验证的绝对普通文件路径直接交给 `RegLoadKeyW`，不要求文件系统 canonicalize；真实 `RegLoadKeyW`、DISM 和 BCDBoot 结果及必要回读才是各自边界的权威事实。高级项在镜像和引导等核心结果完成后发生的可选失败必须有界 warning 并继续，不得把已经可启动的系统倒改成整次安装失败。

第三轮回滚 Extend 的 `0x8004255D` 被微软定义为 `VDS_E_EXTEND_MULTIPLE_DISKS_NOT_SUPPORTED`。把零 `VDS_INPUT_DISK.plexId` 改为已绑定 `IVdsVolume` 的唯一 non-empty simple plex/one member 真实 ID 后，第四轮仍返回同一错误，证明零 plex 不是唯一原因。VDS disk/volume/plex ID 属于 provider/pack 对象关系；全局遍历所有 software provider 后按当前磁盘号取第一个 disk object，可能把另一 pack 的 disk ID 交给当前 volume，使 basic provider 将同盘扩容解释为跨盘。回滚必须从已经 extent 核对的 `IVdsVolume::GetPack` 获取 pack，再仅用该 `IVdsPack::QueryDisks` 返回的唯一当前物理磁盘对象，连同该 volume 的真实 plex ID、精确观察到的增长量和 member index 0 调用 `IVdsVolume::Extend`；无法在同一 pack 唯一绑定时停止，不得拼接不同 provider 的对象 ID。此结论在代码和纯逻辑测试完成后仍需由虚拟机日志跨过原故障位置确认。

### VDS 首选与 Storage Management 回退

LetRecovery 正常端最低兼容 Windows 7，因此缩卷仍以 VDS `IVdsVolumeShrink::Shrink` 为第一选择。微软从 Windows 8 起以 `ROOT\Microsoft\Windows\Storage` 的 Storage Management API 取代 VDS；这只构成 VDS 不可用时的版本化回退，不能把 Windows 7 的主路径删掉，也不能为了探测服务状态在写入前提前失败。

回退边界是 VDS 是否已经返回有效 `IVdsAsync`。在此之前，加载 VDS、绑定目标 `IVdsVolume`、取得 Shrink 接口或启动调用失败，Storage Management 仍会用同一盘符的当前 canonical disk+extent 重新唯一绑定 `MSFT_Partition`；一旦取得异步对象，后续 `Wait`、`Refresh` 或操作后回读错误都可能表示卷已部分变化，此时禁止再调用 `MSFT_Partition.Resize`。两个 provider 的失败必须合并保留，不能丢掉首个错误，也不能转向 DiskPart、PowerShell 或按请求值回滚。

`Resize.Size`、`GetSupportedSize.SizeMin/SizeMax` 都是字节。回退先直接把 VDS desired reclaim 换算为精确最终大小并执行一次真实 `Resize`，真实调用与 canonical 操作后回读优先于支持范围查询。只有 `Resize` 明确返回微软定义的 `4097 Size Not Supported`，且即时回读证明磁盘号、起点和长度完全未变，才调用 `GetSupportedSize`；若 provider 的 `SizeMin` 仍可满足原 minimum reclaim，允许按该字节级值有界重试一次。范围查询失败、返回同一个已经失败的目标、低于 minimum、卷换绑或出现任何实际变化都停止并保留现场。`GetSupportedSize` 的 minimum 由 Disk Defragmenter 和不可移动文件位置决定，所以删掉 `defragsvc`/Optimize Drives 的精简系统必须以真实两套 provider 结果判断，不能仅凭服务缺失预判必败。

2026-08-19 在可丢弃 Hyper-V Gen2、专用 20 GiB 512e VHDX 上完成了四组真实缩卷边界。健康 VDS 主路径从 21,457,010,688 字节精确缩到 17,162,043,392 字节；禁用 VDS 后，`LoadService` 返回 `0x80070422`，Storage Management 仍精确回收 4,294,967,296 字节；保留 VDS 服务定义但离线隔离真实 `vds.exe` 后，首路径返回 `0x80070002`，同一回退仍成功。禁用 `defragsvc` 时 VDS 已返回有效 `IVdsAsync` 后才在 `Wait` 报 `0x80070422`，因此禁止再发第二次 Resize；同时禁用或实际隔离 `vds.exe`、`defragsvc.dll`、`defrag.exe` 和 `dfrgui.exe` 时，Storage Management 返回 provider code 4，前后 extent 完全一致并合并保留两边原因。这最后一种环境没有受支持的可用缩卷引擎，正确结果是在第一次布局写入前停止，而不是猜测尾部或直接截断分区。

同轮测试还复现了一个测试探针揭出的真实产品 bug：`MSFT_Partition.Resize` 已成功完成，但 WMI 代理字段在 `ComApartment` 之后析构，进程在先执行 `CoUninitialize`、再释放 `IWbemServices` 时以 `0xc0000005` 崩溃。Application Error、WER 和本次 PDB 将故障 RVA 精确定位到该析构顺序。VDS 和 Storage Management 持有者现均把 COM interface 声明在 apartment guard 之前，保证接口先释放、最后反初始化 COM；修复后的相同 VDS-disabled 场景退出码为 0，canonical 回读确认精确 4 GiB 回收。测试结束后已恢复指定检查点，并回读两个 Windows 安装中的 VDS/defragsvc 服务及 `vds.exe`、`defragsvc.dll`、`defrag.exe`、`dfrgui.exe` 全部恢复。

### 当前卷到物理盘的身份闭环与异常 provider

当前会话中的 DiskNumber、PartitionNumber 和盘符都是 locator，不是真实身份。微软明确说明 `IOCTL_STORAGE_GET_DEVICE_NUMBER` 的编号只保证到设备移除或系统重启，`MSFT_Partition.DiskNumber` 也可能在重启后改变；`MSFT_Partition.PartitionNumber` 还会随其前方分区布局变化。因此 Shrink、Format 和 Storage Management 回退在首次写入前统一重建 `drive volume handle -> unique volume GUID with same extent -> exact single extent -> current canonical partition -> present physical disk` 闭环，不能把正常端保存的数字直接带进 PE。

物理盘枚举使用 SetupAPI `GUID_DEVINTERFACE_DISK` 的 `DIGCF_PRESENT | DIGCF_DEVICEINTERFACE` 集合，并把每条 device path 当作不透明的 `CreateFileW` locator。所有映射到同一当前 DiskNumber 且可读的 alias 都经自身句柄回读 capacity、canonical layout、GPT/MBR token、`StorageIdAssocDevice` 和 BusType；这些可信来源发生矛盾时返回 `UntrustedStorage`，在 Shrink/Delete/Create/Format/Wipe 之前停止。`IOCTL_STORAGE_GET_DEVICE_NUMBER` 若返回 `ERROR_INVALID_FUNCTION`，仅允许尝试 Windows 10+ 的 `IOCTL_STORAGE_GET_DEVICE_NUMBER_EX` 并严格验证 `Version/Size/DeviceType/DeviceNumber`；两者都失败时拒绝该接口，绝不默认 Disk 0。SMBIOS/HWID 只用于硬件信息页面和虚拟机辅助分类，不参与磁盘写入授权，所以机器码随机化本身不会把写入目标换到另一盘。

Storage Management 回退不直接信任 WMI 返回的 DiskNumber/PartitionNumber。代码先用 canonical extent 找到当前分区号，再要求 `MSFT_Partition` 的 DriveLetter、DiskNumber、PartitionNumber 和 Size 同时唯一匹配；任何冲突都在调用 `Resize` 前失败。BusType 只影响 NVMe 默认值，多个 alias 报告不同 BusType 时同样进入 `UntrustedStorage`，不会用品牌、容量或多数投票猜测。

永久回归现覆盖：legacy device-number `ERROR_INVALID_FUNCTION` 后采用真实 EX 结果、两套 IOCTL 都失败且不得出现 Disk 0、同一 disk number 的 alias capacity/layout/device ID 冲突、BusType 冲突、Storage Management 当前 extent/partition locator 精确匹配，以及 512n、512e、4Kn、520/4160 非整常见扇区几何。2026-08-19 当前代码的 Hyper-V 专用 20 GiB 512e VHDX 再次验证了三条路径：健康 VDS 精确回收 4 GiB；VDS 禁用时 Storage Management 精确回收 4 GiB；`defragsvc` 禁用导致 VDS 已取得 async 后在 `Wait` 返回 `0x80070422` 时不再回退且 extent 未变。VDS 与 `defragsvc` 同时禁用时，Storage Management 返回 provider code 4，测试卷和 80 GiB 系统盘逐分区布局均未变化，两条失败均保留，`non_target_unchanged=true`。

PE 纯逻辑回归还显式把正常端记录的 DiskNumber、磁盘 GUID 和几何全部改掉，证明双系统目标仍只由本次认证随机 marker 重绑定当前 extent；全盘暂存也只按独立 data marker 的当前 extent 重绑定。真实完整安装仍需要由发布 ISO 在 VM 中跨过对应写盘位置才能标记为端到端完成。

双系统 WinPE 只验证 marker 唯一匹配卷的当前 extent 非空、不溢出且不与当前 data/staging 相同或重叠，随后在紧邻首次写盘处确认一次。正常端历史 offset/length、盘符、磁盘号或 GUID 只作诊断，跨启动变化不得阻断；删除预创建卷的可逆 rollback 是独立事务，仍须精确匹配其记录的创建结果。

### 卷枚举和锁定句柄的 Windows 契约

微软官方 API 契约表明，`FindFirstVolumeW`/`FindNextVolumeW`/`FindVolumeClose` 的最低客户端为 Windows XP，因此覆盖 LetRecovery 的 Windows 7 正常端和当前 WinPE；枚举顺序与磁盘号、盘符没有关系，`FindNextVolumeW` 只有在 `GetLastError=ERROR_NO_MORE_FILES` 时表示正常终止。`FindVolumeClose` 只释放搜索句柄；它关闭失败不会改变已完整返回的候选集，不得反过来升级成 locator 失败。`IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`/`VOLUME_DISK_EXTENTS` 至少在 Windows XP 可用，多 extent 需按 `ERROR_MORE_DATA` 扩容缓冲区并重试；安装目标与基础分区事务仍只接受一个非空 extent。marker/journal 使用 `CreateFileW` + `FILE_FLAG_OPEN_REPARSE_POINT` 打开当前对象，不给 `FILE_SHARE_WRITE`/`FILE_SHARE_DELETE`；微软明确说明 sharing mode 在句柄关闭前始终有效，所以候选比较后不得通过路径重开来继续授权。CNG `BCryptGenRandom` 最低客户端为 Windows Vista；当前的 `BCRYPT_USE_SYSTEM_PREFERRED_RNG` 在 Vista 需 SP2，而 LetRecovery 正常端最低为 Windows 7，因此不需要 Win7 回退或更弱的随机源。

## 分散空闲空间不是一个基本分区

正常端可能同时观察到 C:、D:、E: 各有一部分可用空间，但这些文件系统空闲字节不等于一个可创建分区的连续 extent。微软把 `MSFT_Disk.LargestFreeExtent` 定义为磁盘上最大的连续空闲块，并明确说明它也是该磁盘能创建的最大分区；基本卷也只能扩入同盘相邻连续未分配空间。因此不得把多个分区的 `GetDiskFreeSpaceExW` 结果相加后申请一个普通暂存分区。

动态 simple/spanned volume 能把多个 extent 暴露为一个逻辑卷，但需要转换动态磁盘和 LDM 布局；微软同时说明 Windows Setup 不支持动态磁盘，并警告在 WinPE 中创建动态磁盘后，所装 Windows 可能无法使用这些动态盘。LetRecovery 的临时承载不能为了规避一次容量不足而改变整盘存储模型，所以禁止自动转换动态盘、创建跨区卷或 Storage Spaces。

当前安全行为是：只接受一个现有卷或一个真实 Shrink 所释放的单一连续 extent；即使 C:/D:/E: 的诊断性空闲字节合计足够，只要没有单个候选满足精确预算加一次 2 GiB 余量，就必须在正常 Windows、首次写盘和重启之前明确停止。未来若实现多卷承载，必须把它建模为多个独立认证根和逐 artifact 分配，而不是伪装成一个分区；每个单文件仍必须完整容纳于一个根，正常端与 PE 端都要用独立随机 locator 绑定，并为每个自动创建 extent 保留独立回滚/回收事务。

## 备份发布目录共享模式不能阻断系统重命名

2026-08-20 的可丢弃 Hyper-V PE 备份连续三次完成约 6.16 GB WIM 捕获、完整 catalog 校验和全文件哈希，最终都在 `SetFileInformationByHandle(FileRenameInfo)` 的 staged→target 改名处返回 `0x80070020`。关机后只读挂载 AVHDX 证明目标 NTFS 卷上只有本会话 SYSTEM/Administrators 私有目录和完整 `staged.wim`，公共目标名不存在；相同代码对小文件、218 MiB WIM 校验以及 218 MiB 输入的真实并行捕获均能发布，排除了通用 wimlib 生命周期、文件大小、Volume GUID 路径和目标抢占。

微软 `CreateFileW` 契约说明，已有句柄请求 `DELETE` 时，后续打开必须声明 `FILE_SHARE_DELETE`；共享选项会一直持续到句柄关闭。私有目录、目标父目录以及需要被 handle-rename 的 staged/old 文件句柄因此允许 `FILE_SHARE_DELETE`，但仍拒绝 write sharing；目标父目录自身由共享删除的精确句柄保留，只对它的**上级链**保留 no-delete-share 祖先句柄。三轮真实 WinPE 证明句柄 rename 返回共享冲突；第四轮证明持续拒绝 write sharing 的源句柄会让 no-replace `MoveFileExW` 同样返回 `0x80070020`；第五轮在 PREPARED 后释放本程序源句柄仍复现 MoveFileExW 共享冲突，排除了仅由本程序单一文件句柄造成的解释。因此兼容路径先执行 Vista/Windows 7 已支持的 `SetFileInformationByHandle(FileRenameInfo, ReplaceIfExists=false)`，共享冲突时再执行同卷 `MoveFileExW(MOVEFILE_WRITE_THROUGH)`，两者均不允许 replace/copy。若二者都精确返回共享冲突，才对普通文件尝试 Windows XP+ 的同卷 NTFS `CreateHardLinkW`；微软契约明确该 API 只支持 NTFS 文件、所有链接必须同卷且新链接不承载独立安全描述符，因此不得用于目录、ReFS/其它不支持文件系统或目标已存在场景。

所有回退前都必须重开 source 并匹配 retained file ID/对象类型、证明目标父目录同卷且目标不存在；PREPARED 已落盘后仅在实际系统调用瞬间释放拒写句柄。rename 成功后要求 source 消失、target 仍是原 file ID/类型；hard-link 成功后先证明 public/private 两个名字均为原 file ID，再按精确句柄删除 private 名并在 public 名重新取得拒写句柄；调用者随后再次全量匹配原长度/SHA-256。若进程在建链接与删 private 名之间崩溃，恢复只在两个名字的 hash+file ID 都与 journal 的同一 new 对象完全一致时删除 public 名并回滚，任何不同 ID、额外对象或查询失败都保留现场。任一 API 失败后必须重新锁定 source 并证明 file ID 未变才可视为未移动；无法重锁、系统报告成功后的任一回读不确定、路径或字节变化都标记为“可能已变更”，交由精确回滚或 durable PREPARED 恢复。目标抢占、跨卷、路径换绑和不明组合仍失败关闭；受控瞬时释放只兼容真实 VM 证明的系统共享差异，不把长期句柄改成可写，也不降低 no-replace、对象身份和完成态字节约束。

## 手动 PE 维护交接

正常系统端的手动维护入口是独立的 `maintenance` 认证域，不能复用安装、备份或扩容授权，也不创建磁盘 locator/marker。入口默认不暴露；只有本地 `config.json` 明确设置 `pe_maintenance_entry_enabled=true` 且当前不是 PE 时才显示。点击后必须立即创建带动画和当前阶段文字的进度窗口，再沿用私有 PE 根、LRHC1、LRHM3、精确 WIM 快照和一次性 BCD 事务；缺少 PE、复制期间源字节变化、BCD 创建失败或无法安排重启属于核心结果失败，必须停止并在同一窗口保留可诊断错误。

发布目录或受管理缓存中已经存在的 `LetRecovery_PE.wim` 属于用户可定制的本地输入，不得再用下载目录声明的 MD5/SHA-256 或旧大小做运行时门禁。目录哈希只在网络下载 PE WIM 时验证；进入安装、备份、扩容或维护任务后，只保留本次私有复制过程中“源流、私有副本和回读字节一致”的会话内检查，防止复制过程本身得到混合快照，而不要求它等于发布时的旧字节。

BitLocker 恢复密码是可选的受保护启动载荷。正常端只从当前有盘符卷读取系统已经持有的 48 位 RecoveryPassword，去重后写入有大小上限、严格规范化的 `LRBL1` 数据体；公开 config 和 manifest 只保存本次 secret 的长度与 SHA-256，明文只进入 SYSTEM/Administrators 私有的本次启动 WIM。PE 必须先完成 LRHC1 和 LRHM3 认证，再以拒绝 write/delete sharing 的固定 `X:` 文件句柄读取并复核 secret，禁止恢复旧的未认证 JSON、卷标或跨启动磁盘指纹。单卷取不到密码、密码不匹配、FVE 状态查询失败或 `manage-bde -unlock <drive> -recoverypassword <key>` 失败只做有界 warning 并继续维护；不得调用 `manage-bde -off`、移除/暂停保护器或把“进入维护环境”扩大成彻底解密。

维护 purpose 在自动解锁后不构造安装任务，不进入 LetRecovery PE 进度窗口；进程隐藏驻留以维持现有 PE shell 生命周期，用户直接使用 PE 桌面。密钥不得写日志，命令执行器也不得记录带 secret 的参数。是否能看到并操作完整 PE 桌面、锁定数据卷能否按恢复密码自动解锁，仍需用包含 BitLocker 数据盘的可丢弃虚拟机跨重启验证。

## 离线镜像账户分类

Windows Setup 的 `ImageState` 只能说明安装阶段，不能单独证明离线镜像没有用户。账户、无人值守、预装软件和首次登录任务会修改目标状态，因此 fresh 判定必须同时满足：源不是捕获/备份格式；SOFTWARE 中的 Setup State 是 `IMAGE_STATE_GENERALIZE_RESEAL_TO_OOBE` 或 `IMAGE_STATE_SPECIALIZE_RESEAL_TO_OOBE`；离线 SAM 已经实际只读打开并完成库存；库存没有用户自有账户。任一证据读取失败、格式异常或相互矛盾时为 Indeterminate，只跳过这些状态修改，不把已经完成的核心安装改判失败。

只读库存使用微软 `RegLoadAppKeyW`，以 `KEY_READ | REG_PROCESS_APPKEY` 把 SAM 作为应用配置单元打开，不占用全局 HKLM/HKU 名称、不需要 backup/restore privilege，并在根及全部子句柄关闭后自动卸载。Windows 7 同一进程只允许一个 application hive，因此调用必须短作用域且不得嵌套。已初始化 SAM 必须同时解析 `Users\<RID>` 的 V/F 记录与 `Users\Names` 名称索引，两套名称集合大小和内容须按不区分大小写精确一致；单边缺失、损坏、空用户名或越界都算读取失败，禁止靠另一套索引继续猜。

微软原版 `28000.2113.260507-1130` zh-CN Client Pro 镜像给出了重要合法样本：Setup State 为 `IMAGE_STATE_GENERALIZE_RESEAL_TO_OOBE`，其中 8192 字节 SAM（SHA-256 `E2F0655F916A5A4A54885B48528511C5AB649A275E9ADAD2286346C8517A3232`）能由 `RegLoadAppKeyW` 成功加载，但根项为空。这不是“无法读取”，而是 Windows Setup 尚未初始化账户域的泛化模板，库存结果应为成功且零账户。不得因为根项为空报错，也不得把打不开、意外根结构或索引不一致伪装成同一种零账户。

账户归属采用 RID 与一份窄的 Windows-owned 高 RID 名称表共同判断：RID 500–999 是内置账户，不因本地化名称而成为用户；RID 1000 及以上只有在名称不是 `defaultuser0`、`DefaultAccount`、`WDAGUtilityAccount`、`WSIAccount`、`DSMA`、`HelpAssistant`、`HomeGroupUser$` 以及严格数字后缀的 `DWM-*`/`UMFD-*` 等已知系统/安装身份时，才视为用户自有。禁止用“所有 RID>=1000 都是用户”误判官方模板，也禁止把无人值守创建时的保留名称表直接当成已有 SAM 的系统身份表：例如高 RID 的 `Administrator` 或 `NONE` 仍是用户记录。禁止用模糊包含、宽前缀或任意后缀把普通用户名吞进系统账户集合。`IMAGE_STATE_COMPLETE`、捕获源或任一真实用户都必须保留原 SAM、账户和密码。

## 系统卷盘符与上次任务残留

正常系统端的私有 PE 根、BCD `partition=`/`ramdisk=[]` 设备和系统 `boot.sdi` 来源必须由当前 Windows API 返回的系统目录反推出实际系统卷，不得把 `C:` 当成系统卷。LRPE4 journal 只允许其 WIM/SDI 位于该次解析出的同一 `LetRecovery_PE` 根；WinPE 仍按 journal 内绝对路径和卷身份清理，不要求重启后保留原盘符。首登软件计划也不得在生成阶段写入虚构的 `C:` 安装包路径；静态计划使用专用占位符，首次登录脚本再从实际 `%SystemRoot%` 所在卷解析安装器。

新任务开始前，正常端先完成 pending/active journal 的可信 BCD 回滚，再枚举私有根直属项目。只有固定 `boot.wim`/`boot.sdi`、规范 boot GUID 文件和 `ScopedTempFile` 实际十进制 PID/唯一序号命名的载荷可作为孤立产品文件删除；journal 仍只能由事务 parser 处理。未知文件名必须保留且不阻断新任务，目录、重解析点和近似名称不得获得删除授权。不格式化安装在 WinPE 首次目标写入前完成一次当前目标卷身份复核，然后对目标卷同名私有根执行同样的有界清理；格式化和全盘布局路径由其已经授权的文件系统/布局操作自然清除旧目录。备份仍在捕获前清理本次认证载荷，并拒绝把尚存在的私有 PE 根捕获进镜像。

2026-08-20 的全仓 `C:\` 复查移除了正常端 PE、系统组件检查、DISM 定位、驱动目录回退、PE DISM 定位和首登软件计划中的生产盘符假设。保留命中仅用于拒绝危险路径的负向样本、盘符规范化测试、文档命令示例或明确的测试夹具；它们不能进入生产系统卷定位。共享核心 494 项测试和 PE 159 项非提权测试通过，其中包含非 C 系统卷 LRPE4 解析/BCD 路径、孤立文件严格命名、未知文件保留和空产品目录删除。

同日第一轮真实残留回归还暴露了 BCDEdit 返回契约误判：`/delete {GUID}` 已报告成功并确实删除对象，但后续 `/enum {GUID}` 在输出“没有匹配的对象或存储为空”时仍可能以退出码 0 完成。微软只把 `/enum <id> /v` 定义为列出指定对象，并未承诺不存在时一定返回非零；因此对象存在性必须以 stdout 是否包含该完整 GUID 为准。没有完整 GUID 时再独立执行 `/enum {bootmgr} /v` 证明 BCD 存储可读，不解析本地化提示；存储也不可读时仍失败关闭。永久回归覆盖零退出空枚举、其它 GUID、删除后仍存在和存储不可读。修复后的可丢弃 Hyper-V 运行 `b41a9fc1b22d42a8a4482380db5e4092` 先生成有效旧任务并加入精确孤立载荷，再启动第二次 ViaPE 备份；旧 BCD/载荷清理成功，PE 捕获并回到正常系统后生成 6,257,870,370 字节 WIM（SHA-256 `d38648d41a9129b53c35c0df04c4e9de1682bdf73a185a32b02b2d51633ce7a2`），私有 PE 根回读不存在、无 warning，最后自动关机并恢复检查点。这条残留备份链路已经取得跨原故障位置的端到端证据；安装路径的同名目标残留仍由其独立的不格式化写入回归覆盖。

## 2026-08-23 安装矩阵与破坏边界实证

可丢弃 Hyper-V 虚拟机 `LR-W11-UEFI-512e-Latest` 在唯一保留检查点 `ci-uac-disabled-runner-v3-20260820` 上完成了两条正常安装链路。Windows 11 运行 `7b6e83142c454ac5bb3944f7345780a7` 完成 PE、OOBE/首登和终态关机；Windows 10 运行 `05a1f177a03749a0a04cd5897f265ae3` 在改用目标卷固定 `LetRecovery-first-logon.cmd` launcher 后完成首次登录收尾和终态关机。这里的修复依据是 FirstLogonCommands 只保证调用 `CommandLine`；条件判断、重定向、退出码传播和敏感目录清理必须由真实脚本文件承担，不能依赖在 XML 中嵌入复杂 shell 文本。

同一台授权虚拟机是 Hyper-V Generation 2。微软的 Generation 2 支持矩阵明确不支持 Windows 7，原因是 Windows 7 x64 启动依赖 Gen2 不提供的 PIC；两次不同内存配置的真实运行都停在“正在启动 Windows”，没有进入 specialize 或账户阶段。UefiSeven 只补充 GOP/Int10h 兼容，不能提供 PIC，因此这不是继续叠加存储驱动、固定几何检查或放宽安装错误的依据。当前证据只允许把 Windows 7 标记为“受现有 Gen2 平台阻断”；完成 Win7 安装矩阵需要另行授权 Generation 1 可丢弃虚拟机或实体机。

故障注入也跨过了两个真实边界。写前运行 `3bed78062b834c888a92c38cd67d5876` 在创建默认 `boot.sdi` 前停止，CLI 返回失败，原系统、现有控制文件和现有 BCD 保持不变；写后运行 `39a3d56aba364e92986ccbbdfe7eec94` 只接受与认证任务 `SessionId` 精确相同的证据盘 active-run，目标格式化和回读成功后立即注入失败，终态明确记录旧系统回退已禁用，目标 Windows 根已经不存在。两类入口都受显式 `ci-automation` feature 限制，正常 `dev-build` 与正式 PE 构建不包含它们。该回归证明写前失败仍可安全收束，而破坏边界之后必须保留现场、报告重装或人工恢复，绝不能伪造系统级回滚。

2026-08-26 又在同一授权 VM 上把“仅有一个可用系统数据分区、必须先缩出暂存分区”的完整事务放进三道故障边界。正常端 `after_auto_staging` 运行 `f86ab6432d13441a86f6715027110f2b` 把 source extent 从 `78,778,449,408` 缩到 `68,659,707,904` 字节，创建 `10,117,709,824` 字节临时分区后立即失败；产品删除该实际分区、按真实 shrink delta 扩回，并由整盘 canonical 布局逐字节相等、旧内核哈希和无新控制文件共同证明完整回退。PE 写前运行 `0d973cd5f27443748e047ca18f1d9d1e` 先真实复制镜像和 PE 载荷、提交 LRHM3/LRPE4 并重启，完成会话认证与镜像预检后在 `before_target_write` 停止；旧 Windows 根仍存在，离线布局由 4→5→4 精确恢复。PE 写后运行 `d3fbd5c3bba94b07a4b5d0fcf701019e` 让同一 4→5 事务跨过目标格式化和回读后才触发 `after_target_format`；终态必须是 `product_failed` 且包含 `old-system rollback remains disabled`，旧目标 Windows 根为 0，并要求离线布局仍精确等于已提交的 5 分区暂存拓扑。这里“保留暂存”不是清理遗漏，而是破坏边界后保留重装输入和失败证据、禁止用删除暂存和扩回源卷伪造旧系统可恢复。三轮都必须以证据 VHD 全卸载、检查点恢复和 VM `Off` 收束。

## 2026-08-24 可选设备 ID 与安装后暂存回收

VMware Workstation 的实际 `LetRecoveryPE.log` 证明，系统镜像和引导均已完成后，暂存回收在抓取 disk 1 canonical layout 时因 `StorageDeviceIdProperty` 返回 `ERROR_INVALID_PARAMETER (0x80070057)` 被误阻断。这个属性是 SCSI VPD page 0x83 的可选设备级证据，不是每个合法虚拟磁盘或精简 WinPE storage stack 都实现；微软为 `IOCTL_STORAGE_QUERY_PROPERTY` 明确列出 invalid device request、invalid parameter 和 not supported。修复必须先用 `PropertyExistsQuery` 探测，只把对应 Win32 `ERROR_INVALID_FUNCTION`、`ERROR_INVALID_PARAMETER`、`ERROR_NOT_SUPPORTED` 当成属性不可用：GPT/MBR 盘继续由同一个已打开句柄的 capacity、分区表 token 与精确 extents 收束；访问拒绝、其它错误、短结构和损坏 descriptor 仍失败关闭，RAW 盘仍要求设备级 ID。这不是吞掉任意 API error，也不允许退回磁盘号猜测。

可丢弃 Hyper-V 运行 `89e26ff173b0492595d0493dc51f7c87` 在真实 PE 中同样反复得到 `0x80070057`，因此直接跨过了原故障位置。强制 auto-staging 前有 4 个 canonical partitions；正常端把目标 source extent 从 `78,778,449,408` 缩到 `68,717,379,584` 字节，并唯一创建 offset `118,787,932,160`、size `10,060,038,144` 的第五个 GPT basic-data partition。PE 完成镜像、引导、认证暂存删除和源卷扩回，合并日志终态为 `completed cleanup=verified`；关机后的 4 个 partition offset/size 与缩卷前逐字节一致，新系统 `10.0.26100.3037` 和账户 `LRTest11` 经离线实测成立。PE 终态发布还必须区分 `cleanup=verified` 与 `cleanup=incomplete`，清理 warning 不得继续伪装成已经验证回收。

最终受监督运行 `c5669d5d5be54114b3fd3846aa059afe` 重复取得相同的 4→5→4 布局、`0x80070057` 兼容分支、目标 partition 4、新系统/账户与 `cleanup=verified` 证据。产品验收、检查点恢复和 evidence VHD 删除全部完成后，长生命周期 Hyper-V PowerShell 仍可能在进程退出前滞留数 GiB；独立 supervisor 只在本轮 validation 已原子落盘、阶段达到 `evidence_vhd_deleted`、exact evidence VHD 已不存在时，等待 15 秒后回收它亲自启动或按 command line 精确绑定的 host PID，并复用短生命周期 restore helper 已证明的 `VM=Off + 唯一检查点 + 有效 differencing VHD chain` 写出最终 summary。本轮 supervisor 以退出码 0 生成 `passed=true`、`checkpoint_restored=true`、`vm_state=Off` 和 `supervisor_recovered_exit_stall=true`，禁止把这种终态回收泛化到仍在安装、取证或恢复的进程。

## 不格式化重装的个人文件保留与旧系统删除

“保留个人文件”不是完整备份，也不能通过把整个旧系统长期改名来伪装删除。正常端只把当前会话选择写进 LRHC1/HMAC 认证配置，并强制使用包含既有 Windows 的单分区 ViaPE 重装、Windows 7+ WIM/ESD/SWM 和关闭格式化；GHO/XP、空目标、全盘及双系统在正常端写盘前拒绝。PE 在释放 marker 或修改目标前只读枚举 `Users`，保留每个普通本地 profile 的 `Desktop`、`Documents`、`Downloads`、`Pictures`、`Music`、`Videos`；任一保留树出现 reparse、EFS、offline 或 recall-on-access 属性时停止，避免把云占位符、其它卷或无法复制的密文冒充已经本地保留。

六类目录先通过微软定义的同卷 `MoveFileExW(MOVEFILE_WRITE_THROUGH)` 搬入 `LetRecovery_Preserved_<SessionId>`。所有搬移完成前仍属可逆阶段：某次搬移失败时逆序搬回已完成项，只有全部搬回成功才允许复用写前会话回退；任何搬回失败都保留现场并报告 partial state。全部个人目录搬移成功后，第一次删除 Desktop 快捷方式或旧系统对象之前才进入不可逆边界。此后绝不恢复旧引导或旧系统，也不删除保留根。

旧 `Windows`、`Program Files*`、`ProgramData`、剩余 `Users` 等只按固定根级 allowlist 真实删除，未知顶层目录保持不动。快速删除不启动 `cmd`、PowerShell、`del`、`rd` 或 Explorer：使用不排序的 `FindFirstFileExW(FindExInfoBasic, FIND_FIRST_EX_LARGE_FETCH)` 自底向上枚举，绝不进入 reparse point；Windows 8+ 优先用 `SetFileInformationByHandle(FileDispositionInfoEx, DELETE|POSIX_SEMANTICS|IGNORE_READONLY_ATTRIBUTE)`，仅在明确的不支持返回码上清除只读属性并回退 Vista/Windows 7 的 `FileDispositionInfo`。任一打开、枚举或删除 API 失败都报告已经进入的 partial state，不把请求数量或路径猜测成成功。

首登录恢复必须绑定当前 token 的真实 profile 和六个 Known Folder，不能按安装前用户名猜路径。普通文件不能同卷 rename 到新 profile，因为那会保留旧 SID 的安全描述符；也不能误以为 `CopyFileExW` 总会采用父目录身份：微软明确说明 Windows 8/Server 2012 起它会复制源安全资源属性，因此即使事后只重置 DACL，owner 仍会绑定旧账户 SID。当前流程改为 `OpenOptions::create_new` 在当前 token 的真实 Known Folder 中创建目标，让 Windows 在创建边界自然赋予当前账户 owner 和父目录继承 DACL，再从已打开并回读为同一普通文件的保留源句柄流式复制主数据、flush、`sync_all` 并回读类型/长度，最后才删除保留源。创建或写入失败必须删除未提交目标、保留源并让精确 Run 入口下次重试；不得把 `WRITE_OWNER` 或事后安全描述符修补变成正常恢复门槛。

首登 helper 的就绪门禁只能使用微软明确记录为 specialize 与 oobeSystem 已完成的 `ImageState=IMAGE_STATE_COMPLETE`，再结合当前会话 Shell PID、一次有界 `WaitForInputIdle` 和连续稳定窗口。2026-08-26 的真实安装反例中，桌面已出现而后台 helper 长时间不退出，手动重试后资料才恢复；代码检查定位到它额外要求 `OOBEInProgress` 与 `SystemSetupInProgress` 两个瞬态值必须存在且为零。安装完成后这些值可以缺失，把 `None` 当“仍在安装”会制造 30 分钟假等待，现已删除该高误报门禁。旧 Known Folder 内的普通 `desktop.ini` 是目录显示元数据而非用户资料；恢复必须大小写不敏感地从保留源精确删除并保留新 profile 自己的元数据，禁止因同名冲突生成桌面可见的 `desktop (from ...).ini`。同名非普通文件、目录或重解析点仍失败关闭，不得借忽略规则扩大删除对象。

微软的 [Windows Setup States](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/windows-setup-states?view=windows-10) 把 `IMAGE_STATE_COMPLETE` 定义为 specialize 和 oobeSystem 已完成；[FirstLogonCommands](https://learn.microsoft.com/en-us/windows-hardware/customize/desktop/unattend/microsoft-windows-shell-setup-firstlogoncommands) 还明确说明现代 Windows 会异步启动这些命令，不能把命令启动本身误当成 profile 已稳定。修复后的真实 Hyper-V 运行 `0d92b34d7654480d974f124fbb3e9e51` 使用 Windows 11 build `28000.2113` 跨过原故障点：helper 自动等待当前会话 Shell PID `2756`，无需手工重跑便把六类个人资料恢复到 `C:\Users\LRTest11`，随后 cleanup worker 一次删除 staging 与 launcher并自然关机。fixture 同时在旧个人/Public Desktop 写入两个独立 `desktop.ini` marker；离线验收只发现新系统自己的精确 `desktop.ini`，旧 marker 和 `desktop (from ...).ini` 均为零，保留根为零。账户清单、当前 profile、十二个 Known Folder、快捷方式分类、未知根目录、证据 VHD 卸载、唯一检查点恢复和 VM `Off` 全部通过。这一轮把“等待条件”和“元数据过滤”都从单元测试提升为真实跨重启证据。

2026-08-27 的后续 VMware 反例证明，“把 `cmd.exe` 设为临时 Shell”以及后台存在 `cmd.exe`/`conhost.exe` 都不能证明用户实际看到了控制台。现代控制台委派可能只给进程一个非显示用途的伪控制台 HWND；`IsWindowVisible` 也只说明窗口样式带可见位，不证明矩形真正出现在显示器上。更关键的是，微软对 `ShowWindow` 明确记录：启动者提供 `STARTUPINFO` 时第一次 show 命令可能被覆盖。该失败路径现已删除 console gate，改为由 staged `LetRecovery-account-helper.exe --internal-personal-restore-progress-shell <SessionId>` 直接暂代 Winlogon Shell，并以 `WS_POPUP | WS_VISIBLE` 创建自己的全屏顶层 Win32 窗口，再显式调用两次 `ShowWindow`。普通用户正常页面只保留居中的“正在恢复个人文件”和无数值活动条；活动条与 PE 端直接共享 `lr-core::progress_raster` 的 4× 超采样圆角几何、深色轨道和绿色填充，不再显示阶段说明、关机提示、桌面说明或蓝色 marquee。窗口在创建任何 HWND 前启用支持范围内最好的进程 DPI awareness，标题与进度条随显示器 DPI 缩放；每个动画脏矩形先在兼容内存位图内一次性完成背景和进度条合成，再以单次 `BitBlt` 发布，避免直接屏幕绘制暴露中间背景帧。精确 SID 的最高权限交互任务仍是唯一资料写入者，并负责恢复原 Shell、启动系统目录实际 Explorer 和发布同会话 `GetShellWindow` 回执。只有 completed、released 与 `SessionId:ShellPid` 三份回执全部匹配后，进度窗口才退出；失败 marker、窗口资源或 timer 错误必须保持可见错误页，不能退回黑屏。

聚焦 CI 也必须随失败模型更新：不得硬编码 `console_visible=true`，不得只查进程名或缩略图文件存在。短测应使用与生产相同的 helper/runtime 和固定 staging 布局，枚举 helper 的真实顶层 HWND，验证它可见、未最小化、矩形为正且与显示器相交、PID 位于当前非零交互 session，同时确认 Explorer 数为零；发布完成/Shell-release 后再要求唯一 Explorer、稳定的同会话 `GetShellWindow` PID 和 progress helper 已退出。缩略图仍可供人工查看，但在尚未做像素分类时必须明确记录 `thumbnail_pixel_analysis=false`，不得据此声称自动排除了纯黑画面或错误文件管理器。

Desktop `.lnk` 由 PE 优先使用微软 Shell Link COM 契约 `IPersistFile::Load` 后调用 `IShellLinkW::GetPath(SLGP_RAWPATH)`，不调用可能搜索磁盘或弹窗的 `Resolve`。精简 PE 可能没有注册 CLSID_ShellLink，或把原目标跟踪到 PE 的 `X:` 等非目标卷；COM 不可用、未返回文件目标或返回非目标卷时，只按微软 MS-SHLLINK 2.3 严格解析不超过 1 MiB 普通文件中的本地 `LinkInfo`，优先 Unicode 字段，ANSI 回退只接受 ASCII，并拒绝猜测 IDList、网络、环境变量和 ExtraData。COM 已明确定位到旧系统卷 `Users` 时必须保留，二进制回退不得推翻。Shell Link tracking 也可能把原 `C:` 目标显示为 PE 当前分配给离线系统卷的盘符，因此只删除目标为绝对 `C:\` 或当前已认证离线目标卷、且消解 `.`/`..` 后不在对应卷 `Users` 下的快捷方式；目标位于这两个盘符的 `Users`、其它盘、UNC、相对路径、环境变量、无文件目标或无法解析的链接一律保留并在报告中计数。快捷方式删除与旧系统根删除共享同一个不可逆边界，不能因为清理链接失败而声称个人文件搬移已经整体回退。

## 内置 Administrator 密码的私有交接

内置 RID-500 Administrator 的启用状态、目标名称和自动登录选择属于 authenticated config，但密码不能继续作为普通 INI 字段跨重启。真实运行 `c25f56c8da82479ca5b2cc28889f56b7` 在任何重启、删盘或目标写入前停在 `WritePeInstallConfig`，证明原先代码虽然已经为 LRHM3 定义 `ProtectedAdministratorSecret`，正常端注入和 PE 消费链却没有接通；这次停止没有破坏旧系统，也不能通过放宽门禁或把明文写回配置来绕过。

当前单一路径先在正常端完整验证 Administrator 名称、密码和自动登录组合，再把密码序列化为有版本、UTF-8 长度和规范字节形式的有界 secret。authoritative INI 中 `BuiltinAdministratorPassword` 必须为空；LRHM3 只允许一个 `ProtectedAdministratorSecret`，并绑定固定文件名、`ProtectedBoot` 位置、长度和 SHA-256。`pe.rs` 只把这些字节写入本次受保护私有 boot WIM 的 `\LR_AdministratorSecret.txt`，随后从 WIM 解出并逐字节回读，任何缺失或不一致都必须在发布 BCD 前失败。

PE 只从固定 `X:\LR_AdministratorSecret.txt` 以普通非重解析、拒绝并发写入/删除的持有句柄读取；先验证 LRHM3 位置、路径、长度、SHA-256 和规范格式，再把密码注入内存中的 typed install config。公开 INI 含非空密码、启用但没有 secret、禁用却存在 secret、重复记录或文件变化都必须在寻找目标和首次写盘前停止。敏感字符串和临时字节使用 zeroize 容器，不能写入日志、public data 卷或 CI JSON。这个边界与维护模式的 BitLocker secret 复用同一私有 WIM custody 模型，但两种 role 和 purpose 不得互换。

2026-08-25 的可丢弃 Windows 11 build 28000 运行 `9e180d05865a4351b9d508aae4873455` 证明，只把 `AutoLogon` 指向已有的内置 RID-500 账户不能作为跨版本 OOBE 契约。Panther 日志只证明 Setup 启动并结束了 RID helper、密码设置和 AutoLogon 写入；它同时明确记录默认 `WillReboot=Never` 不检查该命令退出码。后续离线 SAM 才是权威结果：RID 500 仍名为 `Administrator` 且处于禁用状态，目标名没有出现。OOBE 因此记录 `accountNeeded = 1`、创建 `defaultuser0`，并由 CloudExperienceHost 清除 AutoLogon，所以 FirstLogon 终结器没有执行。不能再把“RunSynchronous 命令 Finished executing”解释成账户准备成功。

微软 `AutoLogon` 文档明确区分了两件事：Windows 10 上 AutoLogon *may* 跳过 OOBE 账户创建，而通过 `UserAccounts` 创建至少一个账户才是所有版本的明确跨版本后置条件；当 AutoLogon 使用已有账户时，官方也建议同时创建一个 Administrator。因此单一路径仍使用经 authenticated SessionId 派生的唯一 `LrOOBE-<12 hex>` 短命管理员满足 OOBE，但 AutoLogon 必须先指向这个实际存在的临时账户。微软还明确记录：`LogonCount` 大于零时 Windows 会把实际注册表值加一；配置值 1 因而给出两次登录机会，第一次完成 OOBE，第二次由 OOBE 后的原生事务切换给目标 RID-500 名称。不能再把这项兼容行为误当成普通计数直觉，也不能把目标名称提前交给尚未稳定的 OOBE。

同日运行 `9d5dc80c69dc4bc78251fbc75947d9bb` 证明，单独新增临时 OOBE 账户仍不能掩盖 RID-500 helper 失败：Panther 已记录本次唯一 `LrOOBE-939b2dd117dd` 创建成功，但离线 SAM 仍只有被禁用的 `Administrator`、临时账户和 `defaultuser0`，没有 `LRAdmin11`，桌面明确显示自动登录用户名或密码错误。随后运行 `88197ae0f0b9427c841cb8336f5d8d96` 又跨过了 specialize helper：Panther 证明原生 helper 返回 0，最终离线 SAM 却仍显示 RID 500 名为 `Administrator`、临时 `LrOOBE-aa7982b3764a` 存在、`LRAdmin11` 不存在。这个反例证明 OOBE 会在 specialize 成功后重新应用内置账户名称，`WillReboot=OnRequest` 和严格退出码只能证明当时的 API 调用，不能保证 OOBE 结束后的名称。

当前实现因此把 RID-500 准备移动到 OOBE 已完成的第一次临时账户登录。原生 helper 先用 `GetUserNameW` 证明当前 token 属于本次临时账户，以 `NetUserGetInfo` level 4 捕获其 SID并发布规范 marker，再按 RID 500 必要改名、启用并精确回读。不能假定 `Winlogon\DefaultPassword` 会跨过第一次自动登录：真实运行已经证明 Windows 会消费并删除它。因此 PE/Direct 在目标 `LetRecovery_Scripts` 中只暂存已认证规范 secret，specialize 的 SYSTEM helper 用微软 `LsaStorePrivateData` 把它加密写入固定本地 `L$LetRecoveryBuiltinAdministratorPassword`，回读一致后立即删除明文文件；第一次登录再用 `LsaRetrievePrivateData` 取回，不放进参数或日志，把剩余一次 AutoLogon 和固定 HKLM RunOnce 指向目标名称并通过原生 API 安排重启。第二次登录由目标名称创建 `C:\Users\<requested-name>`，helper 必须先证明当前 token 正是该名称，再用 marker 中预先捕获的同一 SID 删除临时 SAM 账户和 `DeleteProfileW` profile；全部首登工作成功后才删除 `DefaultPassword`、AutoLogon 值、LSA secret 和 marker。marker、当前账户、SID、profile、LSA 或注册表回读任一不一致都保留暂存并失败，不允许枚举猜测、删除 `defaultuser0` 或对任意管理员做清理。在新的真实 VM 运行跨过原故障位置之前仍不得宣称动态闭环。

首次接入该两阶段设计的真实运行 `7391e847cd7644868693c2eacacf385f` 在 PE 已完成资料搬移、镜像释放、驱动、更新和引导修复后，于生成无人值守配置时报 `built-in Administrator transition requires both account identities`。取证证明正常端已经传递成对身份，但 PE 仍调用旧的单临时账户包装函数；同时安装后被暂存为 helper 的实际是 PE EXE，因此只给正常端入口增加私有 transition route 也不完整。修复必须沿完整调用图同时完成两件事：PE 生成首登脚本时传入目标名和临时名，PE `main.rs` 也分派 begin/finish/retire；共享生产入口改为一个 `BuiltinAdministratorTransitionAccounts` 成对类型，避免任一端再次编译出“只传一个名称”的调用。该失败发生在不可逆边界后，产品正确发布 `product_failed`、没有声称旧系统回退，宿主完成离线取证后恢复唯一授权检查点并关机。

2026-08-25 运行 `fb6ec21f58bb4831b9754d0caf258613` 再次跨过 PE 镜像校验、个人文件无格式化搬移/清理、镜像释放、驱动、更新和引导，并进入新 Windows 桌面。第一次自动登录账户为本次 `LrOOBE-a06810be7967`；离线 SAM 证明 RID 500 已被 helper 成功改名为 `LRAdmin11` 且启用，临时账户仍存在。收尾日志却以 transition helper exit 1 停止，离线 SOFTWARE 权威回读显示 `DefaultUserName` 仍为临时名、`AutoAdminLogon=1`、`AutoLogonCount=1`，而 `DefaultPassword` 已不存在且目标 RunOnce 未发布。这把故障唯一定位为“首登后读取 Winlogon 明文密码”的错误前提，而不是 RID 改名、用户目录映射、PE 镜像或引导失败。LSA 修复完成后，共享核心 528 passed / 7 ignored、正常端 753 passed / 6 ignored、提权 PE 165 passed；真实 VM 动态闭环仍需下一轮跨过该停止位置。

2026-08-25 后续运行 `4dd4db4823944a549b00e6e7f8992ac2` 已跨过上述位置：当前 token 和 profile 分别精确为 `LRAdmin11`、`C:\Users\LRAdmin11`，RID 500、临时账户清理、六类个人文件及内容、Public 文件和快捷方式分类均经离线证据通过。严格验收只发现固定首登 launcher/staging 残留。日志证明主收尾器已经成功退役 AutoLogon、LSA secret 和 transition marker；Explorer 就绪后带 `-PersonalRestoreAtShell` 再次运行同一脚本时重复调用 retirement，因一次性 marker 已按设计不存在而 exit 1。正确修复不是把退役改成“缺失也成功”，而是保持主阶段的严格失败语义，并让 Explorer worker 成为纯资料恢复阶段，禁止再次触碰账户切换与凭据生命周期。渲染脚本现以 `-not $PersonalRestoreAtShell` 守卫 retirement；下一轮真实 VM 必须同时证明资料恢复成功、staging/launcher 删除和日志无第二次 retirement 失败，才能认定动态闭环。

### 2026-08-26：个人文件必须在 Explorer 之前恢复

上述 Explorer-stage 方案虽然最终能恢复文件，但真实 VMware 日志证明首登脚本在 `18:44:11` 已经启动，Windows Security UI 的同步 WUA 搜索却占满 180 秒超时，个人文件直到桌面已经可见后才开始处理。问题不是 FirstLogon “启动晚”，而是微软只把它描述为桌面前启动，现代实现又会并发运行命令；`RequiresUserInput=false` 也最多只保证约两分钟，不能承担任意长度的资料恢复。

新安装改为 OOBE 后两阶段门禁。普通管理员首次登录只保存并回读原 `HKLM\...\Winlogon\Shell`，为当前精确 SID 注册 Task Scheduler 1.2 LogonTrigger（`InteractiveToken`、`HighestAvailable`、固定 launcher），配置一次性空密码自动登录并立即原生重启；内置 RID-500 模式复用既有临时账户到目标账户的重启，不增加第三次登录。第二次登录由唯一 `cmd.exe /d /c call "LetRecovery-first-logon.cmd" gate <SessionId>` 直接暂代 Shell；最高权限交互任务在同一用户 profile 中先完成账户收束和个人/Public Known Folder 恢复。只有资料完成、原 Shell 原样恢复回读、AutoLogon 退役和 Shell-release 回执全部与本次 SessionId 精确匹配，收尾器才启动并以稳定的同会话 `GetShellWindow` PID 验证 Explorer，再发布 `SessionId:ShellPid` 回执让直接 CMD 退出。可选 SecHealthUI/AppX/软件/Wi-Fi/用户脚本移到放行之后，不再阻塞资料可见性。

Winlogon 启动的 gate 控制台在真实 VMware 中曾未被呈现，造成 Explorer 被正确拦截时只剩无提示黑屏。先后尝试 `start /normal` 和 `CREATE_NEW_CONSOLE` 都没有建立跨 Winlogon/Default desktop 的受支持保证，反而引入外层隐藏 CMD、内层 CMD、用户态可见回执和清理竞态。该“直接 CMD + 只读 PowerShell 阶段文本”方案属于已撤回的中间实现；当前由原生 helper 自己持有真实顶层 HWND，只读受认证回执并绘制最小恢复界面，普通用户进程仍不向 SYSTEM 创建的暂存或日志写任何内容。

2026-08-27 的完整 Hyper-V 失败运行 `d8ca528852f24998a84e205174456b10` 还证明了错误分级的重要性：六类个人文件、Public 文件、快捷方式和保留根均已通过，但普通用户 helper 向 SYSTEM 创建的 `ProgramData\LetRecovery\Logs\FirstLogon-finalize.log` 追加 PID时收到 `ERROR_ACCESS_DENIED`。旧代码把非承重诊断写入失败当成 Explorer 启动失败，每两秒重复创建 Explorer，于是出现主页文件夹和不断刷新的“拒绝访问”。后续运行又证明普通用户向 SYSTEM 暂存写“console visible”回执同样会立即退出并留下黑屏。当前边界是：直接 CMD 全程只读；最高权限交互收尾器独占完成/Shell-release/Shell-verified 回执与日志写入，并且 Explorer 只启动一次、以稳定同会话 `GetShellWindow` PID 收束。旧的双 CMD RunId 只能作为失败样本，不能沿用为当前动态通过证据。

同一次 VMware 复测还发现 `personal_restore_shell_command` 使用 Rust 原始字符串时写入了字面量 `\"`，导致 `Winlogon\Shell` 不是合法的带引号命令；修复真实双引号后，批处理 `start explorer.exe` 虽能打开文件夹窗口，却仍不能证明桌面 Shell 已就绪。这证明“进程存在”不能替代用户可见窗口或 Shell 终态。Shell 与 RunOnce 必须在含空格路径下精确测试且禁止固化 `C:`；进度子进程必须回读 `IsWindowVisible`，Explorer 收尾必须等待同一交互会话中稳定的 `GetShellWindow` PID，普通文件管理器窗口不得误报成功。

该次 PE 日志还重复输出了几十组 `StorageDeviceIdProperty (ERROR_INVALID_PARAMETER)` 与 `StorageIdAssocDevice` 退化 warning。微软将该属性定义为设备 SCSI VPD page 0x83 标识，并明确允许 `IOCTL_STORAGE_QUERY_PROPERTY` 返回 invalid device request、invalid parameter 或 not supported；微软的 DUID 文档也说明许多合法设备不提供 page 0x83。因此当前 VMware 虚拟盘只是缺少可选的设备级 ID，GPT/MBR 写入仍由同一已打开句柄的容量、分区表 token 和精确 extents 收束。底层每次 canonical 回读的确切不支持状态只应进入 debug；用户日志每个当前 disk number 只记录一次退化 warning，禁止把同一可选属性缺失刷成几十个貌似独立的故障。

该修改目前已完成代码、Task Scheduler XML/marker/路由单元测试和正常端/PE 端编译；依据本文件总规则，在新的可丢弃 VM 日志同时跨过“第二登录任务启动、恢复回执、Shell 回读、Explorer 首次出现”之前，只能声明静态与非破坏性验证完成，不能把真实动态闭环标为已验收。

同日第一次真实 VMware 复测又暴露了一个更早的边界：Shell gate、任务、AutoLogon 已完成回读后，请求第二次登录的原生重启仍沿用普通交互重启的 `bForceAppsClosed=FALSE`，Windows 因 `C:\Windows\System32\rgnupdt.exe` 未退出而停在“正在关闭 1 个应用并重启”，必须人工点击“仍要重启”。微软 `InitiateSystemShutdownExW` 契约明确说明，FALSE 会让控制台用户处理阻塞应用，TRUE 才会强制关闭；同时也明确警告 TRUE 可能丢失未保存数据。因此不能把全局重启改成强制，只把两个已经完成账户/AutoLogon/Shell/任务持久化回读、且仍处于 Explorer 放行前的内部 OOBE 过渡改用无人值守强制重启。普通桌面重启和资料写入后的终态关机继续保持温和语义。矩阵必须在个人文件分支看到 `force_apps_closed=true` 的过渡日志，并仍需下一次真实 VM 跨过该重启后才可宣称动态闭环。

## 测试边界

2026-08-29 的 Windows 11 实机日志证明，`SetupGetInfDriverStoreLocationW` 对 73 个已发布 OEM INF 去重累计得到的 3,342,350,118 字节不是 DISM `/Online /Export-Driver` 最终树的大小上界；DISM 成功导出 89 个 INF 后实际普通文件树为 3,605,405,747 字节。旧代码在剩余 78.75 GiB 的数据卷上仍以 `exported_driver_size_exceeded_plan` 失败，属于把只读库存差异错误升级为安装停止。当前设计保留该无临时复制库存用于第一次选盘，但真实导出后用实际树替换同一预算中的驱动项，并以当前 `GetDiskFreeSpaceExW.lpFreeBytesAvailableToCaller` 权威可用字节证明尚未写入载荷加原固定 2 GiB 仍可容纳。权威可用空间查询失败时必须停止并保留底层 Win32 错误；查询成功时差异只记录诊断，空间足够继续，只有实际剩余空间小于剩余确定预算才以容量不足停止。

同日可丢弃 Hyper-V RunId `b7d6425156af45e69e9de83f61294823` 动态证明了当前实现的替换语义：生产 OEM 预估为 64,776 字节，DISM 实际树和来宾独立枚举均为 67,093 字节，导出后 caller 可用空间 20,257,497,088 字节，尚未物化载荷加唯一 2 GiB 余量为 8,209,057,163 字节；安装、目标库存候选、首次启动和清理闭环全部通过。该一次固定 Win10/Win10 PE 虚拟机证据不能替代 Windows 11 实机样本，也不能证明所有真实 OEM 导出规模、物理控制器和磁盘空间组合。

同一批日志还证明，关闭驱动导出后旧 `LetRecovery_Data\drivers` 目录仍被无条件加入 LRHM3，而驱动包中的零字节普通文件被统一 `length > 0` 门禁误报为 `handoff artifact length is outside its limit`。当前任务未请求驱动时必须忽略旧树，不能消费历史目录；请求的目录树允许零字节普通文件，并以空内容 SHA-256、拒写/拒删句柄和路径绑定正常认证。镜像、PCA、marker、secret 和安装器是否必须非空继续由其角色语义单独校验，不能再把承重对象规则泛化到任意文件树成员。

固定 stale-disabled-driver Hyper-V 夹具随后取得两次连续动态通过：RunId `953368ec153e4683ab7531eebf77b8ba` 与 `beb4aac3c60641a296506a5892e9a38b` 均证明 `driver_action=none` 时，261 个残留文件（其中 258 个零字节）、422 UTF-16 单元长路径、目录外噪声和循环链接未进入本次认证 manifest，`PreservedDriver=0` 且本轮 fixture 任意角色命中数为 0，并完成 PE、Win10 首启、`LRTest10` 收尾、自然关机和 Hyper-V 清理。更早 RunId `02f9fdb97eea49b6868b149a07737af0` 已跨过同一 manifest/PE 边界，但新系统首启在 Hyper-V 画面停滞并经授权强制断电，因此仍保留为失败样本；随后两次成功只能说明该停滞未在相同产品输入下复现，不能把它从记录中删除或断言根因已经确定。这些结果只覆盖当前固定夹具，不代表任意损坏树、ACL、第三方过滤驱动、物理存储硬件或启用驱动导出/恢复的路径。

Hyper-V 安装矩阵的宿主进程本身也是需要观测的测试对象。2026-08-24 的成功 Win11 运行在产品验证、证据 VHD 删除和检查点恢复均完成后，长生命周期 Windows PowerShell 仍保留约 3.86 GiB working set；较早 Win10 运行曾达到约 6.92 GiB。证据表明这是 `Restore-VMSnapshot` 后宿主 PowerShell 的退出/内存滞留，不是 LetRecovery 客体泄漏。基线恢复与收尾恢复因此都必须放进短生命周期 helper，并在 helper 退出后重新读取 VM `Off`、唯一授权检查点和有效 differencing VHD chain；主宿主只保留编排状态。

2026-08-25 的个人文件成功分支进一步证明，只隔离检查点恢复仍不够：同一长生命周期宿主在只读挂载两个 VHD、运行 Storage CIM 和离线账户库存之后，已经完成证据复制却能在写 validation 前把 private bytes 从约 160 MiB 持续推到 15 GiB，系统提交率达到 97.6%。这不是测试客体或证据 JSON 体积造成的；对应 JSON 只有约 6–62 KiB。安装矩阵因此把 canonical 布局、只读 VHD 挂载、卷证据和离线账户库存全部移入 `-EvidenceCollectionChild` 短生命周期模式。child 必须先卸载全部 VHD，再原子发布带 RunId、布局数量、唯一系统匹配计数和 `all_vhds_dismounted=true` 的结果；父进程只消费普通 JSON，不接收 Storage CIM 对象。结果已经发布而 child 两秒仍不退出时，父进程可回收该精确 PID并记录 `exit_recovered`，但结果落盘前不得仅凭内存阈值终止。`Add-PartitionAccessPath`/`Remove-PartitionAccessPath` 还必须显式 `Out-Null`，count-only 函数调用方只接受唯一 `Int32`，避免任何 CIM 输出逃逸到整数比较或错误格式化。即使 VHD/Storage 已经隔离，累计约 1,000 个 Hyper-V/CIM 句柄的父进程在进入纯 JSON 后置条件时仍可能继续异常增长；同一批 6–62 KiB 证据在 fresh Windows PowerShell 中逐句回放仅约 36 MiB。因此后置条件现由独立 `-EvidenceValidationChild` 只读执行并原子发布相同 RunId 的 validation，父进程只核对其有界结果后立即进入 VM 收尾。

矩阵宿主应原子发布 `host-stage.json`，至少包含阶段名、PID、working/private/peak working set、句柄数、线程数和 UTC。监督器可据此区分客体未完成、取证未完成、检查点未恢复与宿主已完成但未退出；只有故障模式对应的权威证据已通过、VM 已恢复、evidence VHD 已删除时，才可由独立 finalizer 收束宿主退出停滞。单独超过内存阈值既不能证明失败，也不能授权杀进程，避免在真实磁盘操作仍进行时破坏证据或扩大风险。

实践上，`host-stage.json` 还不足以证明产品验收成功，因为失败流程也必须恢复检查点和删除 evidence VHD。宿主因此要在 Hyper-V 收尾前另行原子写 `validation-result.json`，supervisor 必须同时认证相同 RunId 的 validation 与终态 stage；当前安装链路不再生成、也不应要求从未被调用的遗留 `post-install.ps1/post-install.txt` 收据。安装后的 Windows 识别以当前目标分区上的首次登录完成/自动关机记录和离线账户 inventory 为准，不能写死 ISO build 前缀。

自动测试不得执行真实缩卷、格式化、建分区、清盘、镜像释放或 BCD 写入。最低回归矩阵包括：

- 显式 offset/size 扇区合法但非整 MiB；
- 1 MiB 偏好放不下时回退 provider 原始 extent；
- 既有同盘暂存 offset/length 非整 MiB；
- provider 实际创建范围与请求略有差异但仍被包含且容量足够；
- overflow、越界、缺失和重叠继续拒绝；
- 不同 locator 的旧任务被忽略，唯一同值匹配成功，零个/多个同值匹配失败；
- 双系统只在正常端缩卷和创建，PE 不包含该写路径；
- 全盘计划恰有一个 Windows 目标，暂存所在磁盘不 clean；
- GPT ESP/MSR/Recovery 无盘符时不得阻断同盘暂存接收卷选择；多个已挂载普通卷时选择暂存前结束位置最靠后的卷，完整布局没有合格普通卷时仍失败；
- 全盘使用全部剩余空间时仍保留镜像最低容量，同盘暂存把当前可用末端压到最低值以下时零写入失败；
- 不支持 Range 的远程镜像先下载并读取本地分卷，未知元数据的 80 GiB 回退不得成为下载前容量门禁；18 GiB 展开量得到的 20 GiB 最低值必须贯穿 UI、共享 JSON 和 PE 解析；
- 全盘与双系统确认文案在全部语言中存在且占位符数量一致。
- C:/D:/E: 各有 5 GiB、精确需求为 12 GiB 时，即使合计 15 GiB 也不得返回 Existing 或 ShrinkTarget；测试必须证明没有动态盘或跨区卷回退。
- VDS 主路径成功时不得连接 Storage Management；VDS 在返回异步对象前不可用时，Windows 8+ 才调用 `MSFT_Partition.Resize`，Windows 7 保留原 VDS 错误。
- `Resize` 的 desired 字节值直接成功；`4097` 且回读未变后按非整 MiB `SizeMin` 有界重试；其它返回码、首次调用后范围变化、查询结果不能满足 minimum 或返回相同失败目标时都不得二次写入。
- 个人文件保留必须覆盖：所有目录搬移成功后真实删除旧系统且保留未知顶层数据；后续搬移失败时逆序恢复；删除句柄拒绝 delete sharing 时保留个人文件根并返回不可逆 partial state；`.lnk` 目标在原 `C:` 与 PE 当前已认证离线目标盘符的 `Users` 内、外、其它盘、UNC、环境变量和带 `..` 的分类，以及真实 `IShellLinkW` round-trip；首登只以权威 `IMAGE_STATE_COMPLETE` 判断 Setup 完成、瞬态 OOBE DWORD 缺失不阻断，并丢弃大小写任意的普通 `desktop.ini` 而不覆盖新 Known Folder 元数据。

## 仍需虚拟机验证

- GPT/MBR 全盘重装（含同盘暂存与非整 MiB 尾隙）；
- 双系统完整工作流在禁用/删除 `defragsvc` 的精简系统上的用户可见诊断（底层两套 provider 及 canonical extent 已完成独立 VM 实测）；
- VDS 实际 Shrink 与 create 返回范围和请求略有差异的设备；
- 安装完成后的暂存删除/扩展 warning 终态及双系统启动菜单。
- 正常端启用维护入口后的真实 BCD 重启、PE 桌面可用且 LetRecovery_PE 任务窗口不出现，以及有/无可获取恢复密码时 BitLocker 卷分别自动解锁或保持锁定且不启动解密。
