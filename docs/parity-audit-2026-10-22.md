# AutoCAD 对齐全面审计 — 2026-10-22

分支：`feature/autocad-parity-audit`（基线 `origin/main` 9d2f861，v0.10.3）。
范围：全部引擎命令、菜单、对话框接线、LISP 暴露面。不含 TRIM/点选/拖剪（专项处理中）。

## 方法

1. 枚举全部命令注册表（`cmd::specs` + UI_COMMANDS），对每个命令在富上下文会话
   （直线/圆/TEXT/MTEXT 且全选）实测三条路径：敲命令名 `Session::start`、
   `execute(id, {})`、`execute(id, Null)`，Enter 续跑。探针：
   `crates/engine/tests/parity_audit.rs`（输出表在附录口径下统计）。
2. 通读 `docs/gaps.md`、`docs/parity-checklist.md`（菜单广度 280/491 ≈ 57%，409 个命令）。
3. 对每个"敲名字报错"的命令逐个定位：是上下文未满足、诚实未实现、探针环境差异
   （裸引擎无 IO 钩子）、GUI 已接管、内部子命令，还是真 bug。

## 系统性根因（FIND 的本质）

`Session::start` 对没有 `.interactive()` 的命令直接 `execute(id, Value::Null)` 跑
JSON 形式。凡 JSON 形式有必填参数、又没有 UI 层拦截的命令，**敲名字必然报
"invalid parameters"**——FIND 不是孤例，是一整类。修复模式统一：给命令补
`.interactive()` 提示机（CLI/无对话框环境）+ 需要时补 GUI 对话框，两条路都走
同一个 JSON 形式，保证脚本行为不变。

## 总览数字

| 项 | 数 |
|---|---|
| 注册命令总数 | 434 |
| 敲名字可直接跑通（修复前） | 337 |
| 敲名字报错（修复前） | 97 |
| **F 类：真 bug（本轮已修）** | **24 条命令 + FIND 对话框 + ovimap 选择器接线 + DIST/AREA 的 Arc** |
| 敲名字报错（修复后复测） | 73（全部为下列 A–E 类） |
| A 类：上下文型（与 AutoCAD 一致，非 bug） | 14 |
| B 类：诚实未实现（结构性，gaps 已有条目） | 3 |
| C 类：探针假象（应用内有 IO 钩子可用） | 7 |
| D 类：GUI 已有对话框/选择器接管（含本轮接线的 ovimap 两项，探针看不到 UI 层） | 5+ |
| E 类：内部子命令（JSON 专用，非用户敲入） | 44 |

## F 类真 bug 与修复

| 命令 | 修复前 | 修复 |
|---|---|---|
| `find` | 敲 FIND 报 `'find' is required` | 引擎提示机（查找→替换→全部/逐个，MatchCase/WholeWord 关键字，计数与选中定位）+ JSON 新增 `handles` 作用域 + GUI 查找替换对话框（Edit ▸ Find） |
| `leader` | `'points' is required` | 提示机：逐点 → Enter → 文本行 |
| `base` | `'at' is required` | 提示机：点/Enter 保持当前 INSBASE |
| `breakatpoint` | 需 handle+at | 提示机：选对象 → 点断点 |
| `laymch` | 需 handles+layer | 提示机：选对象 → 点目标层对象 / [Name] 输层名（不存在则报错重输，不像 JSON 形式那样静默建层） |
| `color` | 需 color 参数 | 提示机：名/号/ByLayer/ByBlock/TrueColor→RGB |
| `lweight` | 需 lineweight 参数 | 提示机：数值或关键字 |
| `ltscale` | 同名命令遮蔽裸变量回退 | 提示机：数值，Enter 保持当前值（不走 JSON 默认 1.0) |
| `limits` | 需 min/max | 提示机：两角点 + ON/OFF(LIMCHECK) |
| `style` | GUI 有对话框，CLI 敲入报错 | 提示机：输名设当前（不存在则建）+ `?` 列表 |
| `dimstyle` | 同上且无对话框 | -DIMSTYLE 提示机：输名恢复为当前 / [Save] 存当前变量 / [List]；完整管理器对话框仍缺（gaps F7） |
| `measuregeom` | 需 mode 参数 | 提示机：[Distance/Radius/Angle/Area] 循环回选项，与 AutoCAD 一致 |
| `angle.measure` | 菜单点开无选集回退报错 | 三点提示机（顶点+两点） |
| `radius.measure` | 同上 | 点选圆/弧提示机，选错重选 |
| `datalink` | 需 handle+path | 提示机：选表格 → 输 CSV 路径（立即载入） |
| `wblock` | 需 path | 提示机：名（空=选对象，`*`=整图）→基点→文件名 |
| `ovimap.import` / `ovimap.export` | File 菜单点开无文件选择器 | menus.rs 接 `pick_open`/`pick_save`，跑完回显导入报告 |
| `ovimap.georef` | 菜单点开需参数 | 提示机：纬度/经度（默认当前值，范围校验） |
| `ovimap.push` / `ovimap.pull` | 菜单点开需 projectId | 提示机：输项目 ID（桥接设置仍走 CADCRAFT_OVIMAP_* 环境） |
| `dimtedit.home/left/center/right` | 无选集直接报 "select dimensions" | SelectRun 单选提示机：先提示"Select dimension"（有预选时直接执行） |
| DIST / AREA 的 `Arc` 关键字 | 提示里广告了却回 "not available yet" | 实现：三点定弧（bulge 模型，圆弧段长/扇形面积修正，预览带 bulge） |

## A 类：上下文型报错（AutoCAD 同样如此，非 bug）

`pasteclip`/`pasteblock`/`pasteorig`（剪贴板空）、`refset`/`refclose`（不在
REFEDIT 中）、`bsave`/`bsaveas`/`bclose`（块编辑器未开）、`layuniso`（无隔离层）、
`layerp`（无图层历史）、`mspace`/`pspace`（已在模型页，AutoCAD 同款提示）、
`compare.next`/`compare.previous`（未先 COMPARE）、`dimtedit.*`（无标注选集时；
已补提示机，见 F 类）、`radius.measure`（选集首个非圆/弧时；已补点选机）。

## B 类：诚实未实现（结构性）

| 命令 | 状态 | 原因 |
|---|---|---|
| `attach` | 引擎为桩，回 "not available yet" | gaps FF10：PDF/DGN/DWF 底图未做 |
| `share` | 同上 | 无在线分享服务 |
| `3dorbit` | 同上 | gaps F24：无 3D 实体/轨道导航 |

## C 类：探针假象（应用内可用）

`plot`/`exportpdf`/`import`/`export`/`publish`/`pdfbook`/`document.bytes` 在裸引擎
测试里报 "not available in this build"——IO 钩子是宿主启动时经
`file::io()` 的 OnceLock 注册的，桌面/CLI 应用内正常。属测试环境差异，不是缺陷。

## D 类：GUI 已接管（敲入在应用内走对话框/选择器）

`open`、`qsave`、`saveas`、`recover`（文件选择器）；`export`/`import`/`publish`/
`preview`（exchange::route）；`plot`/`print`、`style`、`linetype`、`ddptype`、
`tablestyle`、`mleaderstyle`、`pagesetup`、`battman`（managers::route 对话框）；
`qselect`、`find`（本轮新增）。本次给其中 CLI 仍报错的 `style` 补了引擎提示机。

## E 类：内部子命令（JSON 专用，AutoCAD 里也没有对应敲入命令）

`document.switch`、`view.set`、`vports.activate`、`blocks.dynamic`/
`blocks.reference`/`blocks.reference.set`/`blocks.grips`/`blocks.grip`、
`grip.move`/`grip.stretch`/`grip.rotate`/`grip.scale`/`grip.mirror`/`grip.option`、
`layer.new`/`layer.set`/`layer.delete`、`layerstate.save`/`restore`/`delete`/
`rename`、`layout.delete`/`rename`/`copy`、`table.set`/`insertrow`/`deleterow`/
`insertcol`/`deletecol`/`merge`/`unmerge`/`cellstyle`/`formula`、
`style.rename`/`delete`/`current`、`dimstyle.rename`/`delete`/`current`/
`override`、`getvar`、`lisp`、`viewport.set`。这些是对话框与脚本调用的编程面，
参数由调用方给齐；敲入报错属预期。

## 提示层桩（回显 "not available yet"，均非崩溃，属行为不同/缺失）

| 位置 | 内容 | 处置 |
|---|---|---|
| -ATTEDIT 逐个编辑 | Position/Height/Angle/Style 子选项、色彩簿 | 未做：逐个编辑器已有其余子选项；色彩簿需 .acb 数据文件（仓库无此资产，见下） |
| -LAYER | 色彩簿选色 | 同上 |
| -RENAME | 材料/打印样式/视口配置改名 | 未做：冷门；图层/块/样式等主表改名可用 |
| MVIEW | Restore 命名视口配置到图纸空间 | 未做：gaps F15（2–4h），命名视口配置本身可用 |
| PLINE/数组夹点 | 数组夹点编辑、Key points 模式 | 未做：gaps 已列 |
| MLEDIT | Merged Tee 合并 | 未做：其余 11 种 MLEDIT 模式可用 |
| SPLINE | CV 控制点编辑模式 | 未做：拟合点模式可用 |
| OBJECTISOLATIONMODE=1 | 隔离状态跨会话持久 | 未做：会话内隔离可用 |

## 未做清单（及原因）

1. **DIMSTYLE 完整管理器对话框**——gaps F7 估 10–15h；本轮已给 -DIMSTYLE 提示机
   覆盖敲入路径，菜单点开走提示流。对话框留待 F7 专项。
2. **COLOR / LWEIGHT 的 GUI 对话框**——AutoCAD 是对话框；本轮补了提示机（敲入与
   CLI 可用），GUI 点开 Format ▸ Color… 走提示流。独立选色对话框（含色彩簿/
   真彩色页签）未做：色彩簿需要 .acb 资产文件，仓库按 clean-room 规则不能带
   Autodesk 的色彩簿数据。
3. **高德注记叠加层**（ovimap `amap-ann`）——CadCraft 无"注记叠加层"概念，
   导入会当底图。需要瓦片图层合成（多层叠加渲染）这一新功能，非审计修复范围；
   已在 gaps 语境下记录为图层合成缺口。
4. **3D / 图纸集 / 云类缺失**（3DORBIT、3D 实体编辑、Sheet Set、Vault/云协作）——
   gaps F23/F24 结构性条目，非本轮可修。
5. **MVIEW Restore、-RENAME 冷门表、SPLINE CV、数组夹点**——见上表，均为单点
   小功能，工作量与收益不匹配本轮"先消灭不能用"的优先级，已逐项登记不静默。

## 验证

- 探针复测：修复后 `start` 报错数从 97 降至 73，减少的 24 条与 F 类清单逐条
  对应；探针已升级为断言型回归门（24 条已修命令必须 start 成功，总报错数
  不得超过 73）。
- 新增测试：`crates/engine/tests/find_command.rs` 5 个、
  `crates/engine/tests/typed_fallbacks.rs` 20 个，全过（覆盖每个新提示机的主
  流程，含 DIST/AREA 的 Arc 数值断言：半圆弧长 5π、方+半圆面积 139.2699）。
- 回归：引擎 lib 单测 312 通过，相邻集成测试全过（inquiry_keywords 4、
  layer_cmdline 3、wblock_base 1、table_breaks 3、dimtedit_prompts 2、gis 16、
  measure_distance_snapping 14、props_bad_lineweight 1、rename_cmdline 2、
  prompt_keywords 3、setvar_coverage 4），0 失败。inquiry_keywords 有一条既有
  测试原以 "a"（→Arc）断言 "not available yet"，因 Arc 已实现而改为断言新
  行为（进入 on-arc 提示），测试意图（广告关键字都要能用）不变。
- `cargo fmt` 已执行；`cargo clippy -p cadcraft-engine -p cadcraft-ui-egui
  --all-targets -- -D warnings` 0 错误。

## 附：FIND 的 AutoCAD 对照

| 能力 | AutoCAD FIND | 本轮后 CadCraft |
|---|---|---|
| 查找文本 | ✓ | ✓（提示机第一问 / 对话框） |
| 替换文本 | ✓ | ✓ |
| 全部替换 | ✓ | ✓ |
| 逐个替换/跳过 | ✓ | ✓（[Replace/Skip/All/Quit]） |
| 区分大小写 | ✓ | ✓（MatchCase 关键字/复选框） |
| 全字匹配 | ✓ | ✓（WholeWord 关键字/复选框） |
| 结果计数 | ✓ | ✓（"N match(es) in M object(s)"） |
| 定位到对象 | ✓（Zoom to） | ✓（选中 + 对话框 Find Next 缩放到对象） |
| 搜索范围=选集 | ✓ | ✓（JSON `handles` / 对话框按当前定位逐个替换） |
| 通配符/正则 | AutoCAD 仅通配符选项 | 未做（引擎为子串匹配，gaps 登记） |
