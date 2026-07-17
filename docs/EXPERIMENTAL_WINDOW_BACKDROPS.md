# 正常系统端实验性窗口背景

此功能只影响正常 Windows 系统端，不会写入或改变 PE 端界面。默认关闭；需要在程序同目录的 `config.json` 中设置：

```json
{
  "experimental_window_backdrop": "mica"
}
```

可选值：

- `none`：关闭并使用原有不透明背景（默认）。
- `auto`：让 DWM 自动决定系统背景，可能只影响标题栏或不显示效果。
- `mica`：长驻主窗口背景；Windows 11 当前对应 Mica。
- `acrylic`：瞬态窗口背景；Windows 11 当前对应 Desktop Acrylic。
- `mica_alt`：标签式窗口背景；Windows 11 当前对应 Mica Alt。

实现只调用公开的 `DWMWA_SYSTEMBACKDROP_TYPE` 与 `DwmExtendFrameIntoClientArea`。这些系统背景类型最低支持 Windows 11 build 22621；旧系统、关闭 DWM 合成、节能/辅助功能策略或 DWM 拒绝请求时，程序会记录警告并回退原有背景。未知配置值也按 `none` 处理，不会导致整个配置文件失效。

明暗主题启用 `mica`、`acrylic` 或 `mica_alt` 后，主窗口基础层与导航基础层都使用 DWM 的黑色玻璃键，材质覆盖完整客户区。普通导航和次要按钮直接使用原始设计的蓝灰预乘 Alpha 表面，使壁纸、窗口激活状态和 DWM 材质变化仍能透过按钮；悬停、按下和禁用态使用独立但克制的叠加色与透明度，选中导航和主操作按钮仍保持不透明强调色。Edit、闭合 ComboBox、ListBox、ListView 和表头共享同一材质状态定义；经典 Win32 子 HWND 无法可靠地把逐像素 Alpha 发布到父 DWM 表面，因此使用该覆盖层在 Windows 11 Mica 中性基底上的确定性合成色，避免退回不相干的实体灰，也避免白角、黑角和浅色模式崩溃。按钮保留原有 4 倍 GDI `RoundRect`，字段和列表外框使用固定 8×8 子像素覆盖率绘制同半径圆角；原生输入、光标、键盘和无障碍语义保持不变。

浅色全客户区不是通过把黑色改成近黑色实现的。透明 STATIC 标题使用 UxTheme 的 `DTT_COMPOSITED` 玻璃文字路径；实体按钮、复选/单选标题和闭合 ComboBox 使用白色 GDI 字形遮罩重着色为预乘 BGRA，其中 ClearType RGB 遮罩按三通道平均覆盖率转换，保留普通 400 字重而不把导航与按钮伪画成粗体；浅色材质复选框继续逐像素使用固定 Windows 11 主题资源的原始图元，只把透明和半透明边缘与 Windows 11 中性材质回退色预混合，并把真正不透明的纯黑笔画与 DWM 黑色玻璃键区分开，不改变半径、勾号、状态或配色；ListView 自绘行则在已知实体背景上使用不透明 GDI 文字，保留与原生列表一致的 ClearType 子像素渲染。Edit、ComboBox、ListBox 和 ListView 不再使用圆角 HRGN；`CreateRoundRectRgn` 和 `FrameRgn` 的二值边缘会裁掉部分覆盖率像素，在直边与圆弧连接处产生阶梯、颗粒或 C 形断口。库存客户区绘制完成后，统一的 8×8 覆盖率外框最后写入：全外角恢复 DWM 黑色玻璃键，部分覆盖像素通过 32 位顶向下 BGRA DIB 发布预乘 Alpha，直边和圆弧使用同一个绝对边框色；ComboBox 的矩形窗口区域只隐藏 USER32 保留的展开高度。浅色材质使用可辨识的一像素描边，空 ListView 显式填满完整客户区。每次 GDI 将字形画入 DIB 后都先调用 `GdiFlush`，再读取或改写 DIB bits，避免延迟 GDI 批处理在真实多行列表首帧中破坏 USER32 绘制状态。微软对 Mica 作为控件后方基础层、系统背景、整张玻璃、GDI 区域和 GDI 批处理同步的说明可参考：[Mica material](https://learn.microsoft.com/windows/apps/design/style/mica)、[DWM_SYSTEMBACKDROP_TYPE](https://learn.microsoft.com/windows/win32/api/dwmapi/ne-dwmapi-dwm_systembackdrop_type)、[DwmExtendFrameIntoClientArea](https://learn.microsoft.com/windows/win32/api/dwmapi/nf-dwmapi-dwmextendframeintoclientarea)、[SetWindowRgn](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-setwindowrgn)、[CreateRoundRectRgn](https://learn.microsoft.com/windows/win32/api/wingdi/nf-wingdi-createroundrectrgn) 与 [CreateDIBSection](https://learn.microsoft.com/windows/win32/api/wingdi/nf-wingdi-createdibsection)。工具对话框和 PE 端始终使用普通不透明背景。

扩展玻璃客户区中的 Edit `HBRUSH` 使用“目标合成色减去 Mica 中性基底”得到的 RGB 贡献量。DWM 把黑色视为玻璃键；若画刷直接使用已经合成的字段色，会把材质基底重复相加，使深色 Edit 从目标 `#3C4660` 变亮到 `#5C6680`。ComboBox、ListBox 和 ListView 的不透明 BGRA/主题颜色路径不使用这项画刷补偿。闭合 ComboBox 的正文、箭头区和圆角外框始终从同一次状态查询取得 normal/hot/dropped 表面色，禁止外框用常态色而正文用展开色，否则抗锯齿内沿会产生白色弧线。悬停、聚焦和展开状态还必须把圆角外框同步切换为当前明暗调色板的强调色；浅灰常态描边即使像素来源正确，叠在浅蓝交互表面上仍会被看成白弧，因此不能作为交互态描边复用。Windows 10/11 的库存 ComboBox 在样式边框移除后仍可能保留主题客户区内缩；同步闭合重绘必须通过 window DC 覆盖整个可见字段后再画唯一外框，不能只用 client DC 留下两像素的库存灰白顶边。真实鼠标点击还会进入 USER32 的嵌套下拉跟踪循环，库存主题可能在按下消息返回前再次覆盖闭合框；实现必须把一次同步重画投递到该嵌套循环中，并用真实鼠标点击截图回归，不能只用 `CB_SHOWDROPDOWN` 直接消息验证。

系统背景不是窗口透明度：程序不保留 `WS_EX_LAYERED`，也不使用 `WS_EX_TRANSPARENT`。首帧防白闪暂存使用非零 alpha，并在同步绘制完成后立即清除分层样式和刷新命中测试缓存，避免点击穿透到窗口后方。
