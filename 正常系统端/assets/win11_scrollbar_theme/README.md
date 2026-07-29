# Windows 11 scrollbar theme resources

These PNG strips are exact `IMAGE` resources extracted from the Windows 11 theme files used by
the project's native UI reference environment. They are not redrawn approximations.

| Mode | Source file | Source SHA-256 | `ARROWBTN` | `THUMBBTNVERT` | `UPPERTRACKVERT` / `LOWERTRACKVERT` |
| --- | --- | --- | --- | --- | --- |
| light | `aero.msstyles` | `53C27FF7E1236848B19375579E531B31AC57EF7A18127B7392E71248E9621E8B` | 1012–1016 | 1022–1026 | 1032–1036 |
| dark | `Dark.msstyles` | `CE27B98350E6FB43ECAB6C9B3A6D1B8CEF7FC8F3F8431C797ADE5292E6EBA0D1` | 894–898 | 904–908 | 914–918 |

The five files for each mode correspond to the theme's DPI thresholds 96, 120, 144, 192 and 240.
The thumb and track files are vertical strips of five equal-height states in the order defined by
`vssym32.h`:
`SCRBS_NORMAL`, `SCRBS_HOT`, `SCRBS_PRESSED`, `SCRBS_DISABLED`, `SCRBS_HOVER`.
The arrow files retain the source strip's 20 `ABS_*` states. `build.rs` selects Up states
0/1/2/3/16 and Down states 4/5/6/7/17 as
Normal/Hot/Pressed/Disabled/Hover.

The source properties are `IMAGESELECTTYPE=IST_DPI`, `IMAGELAYOUT=IL_VERTICAL`,
and `SIZINGTYPE=ST_STRETCH`. The original sizing margins are 1/7/1/7 for the thumb, 1/0/1/0
for both track parts, and 1/1/1/1 for the arrows. `build.rs` preserves those rules, slices all
states, and premultiplies the extracted alpha before generating the embedded BGRA tables.
`ARROWBTN` and `THUMBBTNVERT` are also marked `TMT_TRANSPARENT=true` in both source themes.
Because the resource extractor stores their colour-key surface as opaque PNG pixels, `build.rs`
recovers each state frame's uniform corner key and converts exact matches to alpha zero before
premultiplication. The extractor's DPI scaling can leave a one-level colour fringe next to that
key (`#212121` around the dark `#202020` host or `#fefefe` around white). The build step therefore
also removes only near-key pixels connected to a frame edge; interior pixels of the same colour
remain untouched. Track images are formally opaque, but their frames include the exact source
host surface (`#202020` or white) around the actual track. The build step removes that matching
per-frame corner colour as a composition adaptation while retaining every real track pixel. This
prevents the source host from becoming a full-height outer capsule on the differently coloured
advanced page.

At run time the native scrollbar model still owns the range, keyboard access, and accessibility,
but its non-client pixels are clipped out and `SetScrollInfo` never requests a stock redraw. A
sibling overlay draws both extracted track halves followed by the arrow and thumb glyphs. Pointer
hits and dragging use `GetScrollBarInfo` geometry and `GetScrollInfo` ranges before being routed
back through the existing `WM_VSCROLL` state machine. In Normal only `THUMBBTNVERT` is drawn,
producing the compact line; arrow resources remain hidden. On hover the active component uses Hot
and the other visible glyphs use Hover. Pressing changes only the active component to Pressed;
disabled scrollbars hide arrows and use the Disabled thumb. The expanded track remains continuous
from the top arrow through both sides of the thumb to the bottom arrow without carrying the source
host surface around it.

State and pointer changes are coalesced into at most one queued frame, with the newest thumb
position replacing older pending positions. On a normal desktop the complete opaque BGRA frame is
published through D3D11, Direct2D, and one DirectComposition `Commit`; hardware device creation
falls back to WARP. All graphics DLLs are loaded from System32 at run time so reduced WinPE images
can safely use the fallback without a load-time dependency. If any composition stage fails, the
only fallback painter runs inside `BeginPaint`/`EndPaint`, composes the complete frame in a
compatible memory DC, and publishes it with one `BitBlt`. No path writes through `GetWindowDC` or
exposes a background-only or partially assembled intermediate frame.

Microsoft documents the state indexing, component geometry, and theme image behavior in
[Parts and States](https://learn.microsoft.com/windows/win32/controls/parts-and-states),
[Property Identifiers](https://learn.microsoft.com/windows/win32/controls/property-typedefs), and
[SCROLLBARINFO](https://learn.microsoft.com/windows/win32/api/winuser/ns-winuser-scrollbarinfo).
