# 正常系统端实验性窗口背景

此功能只影响正常 Windows 系统端，不会写入或改变 PE 端界面。默认关闭，可在“关于”页勾选“启用 Mica（实验性）”；程序会同步保存到同目录的 `config.json`：

```json
{
  "experimental_window_backdrop": "mica"
}
```

配置只保留两个值：

- `none`：关闭并使用原有不透明背景（默认）。
- `mica`：长驻主窗口背景；Windows 11 当前对应 Mica。

`auto`、`acrylic`、`mica_alt` 是早期实验值，现已停止支持；读取这些历史值或任何未知值都会安全回退 `none`，不会导致整个配置文件失效。实现只调用公开的 `DWMWA_SYSTEMBACKDROP_TYPE=DWMSBT_MAINWINDOW` 与 `DwmExtendFrameIntoClientArea`。该系统背景类型最低支持 Windows 11 build 22621；旧系统、关闭 DWM 合成、节能/辅助功能策略或 DWM 拒绝请求时，程序会记录警告并回退原有背景。

明暗主题启用 `mica` 后，主窗口、导航基础层和正常端工具对话框都使用 DWM 的黑色玻璃键，材质覆盖完整客户区。普通导航和次要按钮直接使用蓝灰预乘 Alpha 表面；深色常态表面提高覆盖率，避免壁纸把控件冲得过透，浅色常态表面使用非纯白的冷灰低覆盖层，避免退化成纯白矩形。悬停、按下和禁用态使用独立但克制的叠加色与透明度，选中导航和主操作按钮仍保持不透明强调色。Edit、闭合 ComboBox、ListBox、ListView 和表头共享同一材质状态定义；经典 Win32 子 HWND 无法可靠地把逐像素 Alpha 发布到父 DWM 表面，因此使用该覆盖层在 Windows 11 Mica 中性基底上的确定性合成色。按钮保留原有 4 倍 GDI `RoundRect`，字段和列表外框使用固定 8×8 子像素覆盖率绘制同半径圆角；原生输入、光标、键盘和无障碍语义保持不变。

浅色全客户区不是通过把黑色改成近黑色实现的。主窗口和工具壳的直接 STATIC 标题继续使用 UxTheme 玻璃文字缓冲，避免给标签矩形铺上实体底色；工具内容容器中的嵌套 STATIC 会在经典子 HWND 重定向时丢失这份文字 Alpha，因此仅对嵌套标签使用确定性的白色 GDI 字形遮罩并重着色为预乘 BGRA。两条路径保持同一字重和明暗文字色，标签矩形本身都透明。实体按钮、复选/单选标题和闭合 ComboBox 使用白色 GDI 字形遮罩重着色为预乘 BGRA，其中 ClearType RGB 遮罩按三通道平均覆盖率转换，保留普通 400 字重而不把导航与按钮伪画成粗体；浅色材质复选框继续逐像素使用固定 Windows 11 主题资源的原始图元，只把透明和半透明边缘与 Windows 11 中性材质回退色预混合，并把真正不透明的纯黑笔画与 DWM 黑色玻璃键区分开，不改变半径、勾号、状态或配色；ListView 自绘行则在已知实体背景上使用不透明 GDI 文字，保留与原生列表一致的 ClearType 子像素渲染。Edit、ComboBox、ListBox 和 ListView 不再使用圆角 HRGN；`CreateRoundRectRgn` 和 `FrameRgn` 的二值边缘会裁掉部分覆盖率像素，在直边与圆弧连接处产生阶梯、颗粒或 C 形断口。库存客户区绘制完成后，统一的 8×8 覆盖率外框最后写入：全外角恢复 DWM 黑色玻璃键，部分覆盖像素通过 32 位顶向下 BGRA DIB 发布预乘 Alpha，直边和圆弧使用同一个绝对边框色；ComboBox 的矩形窗口区域只隐藏 USER32 保留的展开高度，并在裁剪后显式重绘刚暴露的父窗口尾部，不能让旧 UxTheme 像素残留成字段下横线。浅色材质使用可辨识的一像素描边，空 ListView 显式填满完整客户区。每次 GDI 将字形画入 DIB 后都先调用 `GdiFlush`，再读取或改写 DIB bits，避免延迟 GDI 批处理在真实多行列表首帧中破坏 USER32 绘制状态。微软对 Mica 作为控件后方基础层、系统背景、整张玻璃、GDI 区域和 GDI 批处理同步的说明可参考：[Mica material](https://learn.microsoft.com/windows/apps/design/style/mica)、[DWM_SYSTEMBACKDROP_TYPE](https://learn.microsoft.com/windows/win32/api/dwmapi/ne-dwmapi-dwm_systembackdrop_type)、[DwmExtendFrameIntoClientArea](https://learn.microsoft.com/windows/win32/api/dwmapi/nf-dwmapi-dwmextendframeintoclientarea)、[SetWindowRgn](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-setwindowrgn)、[CreateRoundRectRgn](https://learn.microsoft.com/windows/win32/api/wingdi/nf-wingdi-createroundrectrgn) 与 [CreateDIBSection](https://learn.microsoft.com/windows/win32/api/wingdi/nf-wingdi-createdibsection)。PE 端始终使用普通不透明背景。

扩展玻璃客户区中的原生单行 Edit 不使用 `WS_EX_LAYERED` 或 `SetLayeredWindowAttributes`。微软说明分层窗口由系统保存离屏图像，并不保证窗口内容重新暴露时再次请求绘制；这类子窗口因此不能满足“首次显示和主题切换后不依赖鼠标消息”的要求。程序在每次主题应用时主动清除旧版本可能遗留的分层样式，保留 USER32 原生文字、光标、选择、IME、键盘、命中和无障碍语义，并让 Edit 与 ComboBox、ListView 一起进入窗口的同步首帧及主题重绘事务。为了让浅色黑字不被全客户区 DWM 玻璃键吞掉，Edit 的 `WM_PAINT` 用顶向下 `BeginBufferedPaint` 缓冲调用原生 `WM_PRINTCLIENT`，缓冲事务中的 `WM_CTLCOLOREDIT` 使用已解析字段色和精确 `palette.text`；`GdiFlush` 后调用 `BufferedPaintSetAlpha(..., 255)`，一次性提交不透明的背景、文字和选择，而不是把 Edit HWND 变成分层窗口。缓冲不可用时才回退普通 GDI 贡献色。该子控件不安装圆角 HRGN，外角恢复父窗口玻璃键，再由统一 8×8 覆盖率路径绘制圆角外框。ComboBox、ListBox 和 ListView 的不透明 BGRA/主题颜色路径不使用 Edit 画刷补偿。闭合 ComboBox 的正文、箭头区和圆角外框始终从同一次状态查询取得 normal/hot/dropped 表面色，禁止外框用常态色而正文用展开色，否则抗锯齿内沿会产生白色弧线。悬停、聚焦和展开状态还必须把圆角外框同步切换为当前明暗调色板的强调色；Windows 10/11 的库存 ComboBox 在样式边框移除后仍可能保留主题客户区内缩，因此同步闭合重绘必须通过 window DC 覆盖整个可见字段后再画唯一外框。真实鼠标点击会进入 USER32 的嵌套下拉跟踪循环，库存主题可能在按下消息返回前再次覆盖闭合框；实现必须把同步重画投递到该嵌套循环中，并用真实鼠标点击截图回归，不能只用 `CB_SHOWDROPDOWN` 验证。微软对玻璃上 GDI 缓冲 Alpha 的说明见 [BufferedPaintSetAlpha](https://learn.microsoft.com/windows/win32/api/uxtheme/nf-uxtheme-bufferedpaintsetalpha)。

系统背景不是窗口透明度：主窗口和工具对话框不使用 `WS_EX_LAYERED` 或 `WS_EX_TRANSPARENT` 首帧暂存。控件树在隐藏状态完成字体、主题和布局准备，显示后同步重绘完整非客户区、客户区与全部子控件；这样顶层窗口从第一帧起就是普通不透明命中目标，导航点击不会穿透到窗口后方。主窗口使用官方 `WS_EX_COMPOSITED` 后代双缓冲；页面、语言和主题切换只暂停可见顶层根窗口，完成文字、库存、显隐和布局后再以一次 `RedrawWindow(...RDW_ALLCHILDREN | RDW_UPDATENOW)` 发布由下到上合成的完整控件树。页面切换只更新当前页和公共页头，不再重复布局所有隐藏页面；常用材质圆角图元由有界线程本地缓存复用，避免切页时重复构造相同位图。微软关于 Mica 基础层、分层窗口命中与批量重绘的说明见 [Apply Mica in Win32 apps](https://learn.microsoft.com/windows/apps/desktop/modernize/ui/apply-mica-win32)、[Extended Window Styles](https://learn.microsoft.com/windows/win32/winmsg/extended-window-styles)、[Layered Windows](https://learn.microsoft.com/windows/win32/winmsg/window-features)、[SetLayeredWindowAttributes](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-setlayeredwindowattributes)、[WM_SETREDRAW](https://learn.microsoft.com/windows/win32/gdi/wm-setredraw) 与 [RedrawWindow](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-redrawwindow)。
