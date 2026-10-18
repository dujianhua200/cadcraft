# 滑洲云图 ↔ CadCraft 联动设计（MVP）

> 状态：设计文档 v1.0（2026-10-14）。只定义格式、投影、映射与接口，不含实现代码。
> 两份内容一致，分别存放于 `cadcraft-gis/docs/ovimap-link.md` 与
> `ovimap/docs/cadcraft-link.md`；以后改一处必须同步另一处（保持字节一致，
> 可用 `diff` 校验）。本文档不修改任何代码。
>
> 核实基线（只读核查，2026-10-14）：
>
> - ovimap：`~/workspace/ovimap`，分支 `rebuild/ovi-ux-v4`。
>   标记/轨迹统一为 `MapLabel`（`lib/models/map_label.dart`，注释“一律以 WGS-84 存储”）；
>   KML 导出用 `buildLabelChains`（`lib/export/kml.dart`）；
>   地图用 flutter_map + latlong2；瓦片缓存 `lib/services/tile_cache.dart`。
> - CadCraft：`~/workspace/cadcraft-gis`，分支 `feature/gis-link`，tip `2a603ef`。
>   现无 GeoJSON / KML / GPX / 瓦片 / WGS84 / UTM / 地理参照支持（grep 核实）。
>   已有：实体 xdata（`crates/doc/src/entity.rs`，`Common.xdata`）、
>   外来对象/XRECORD 透传（`crates/doc/src/foreign.rs`）、
>   控制协议（`docs/control-protocol.md`）与 MCP（`docs/mcp.md`）。

## 0. 目标与非目标

### 0.1 目标（用户原话：像奥维互动地图一样）

- 地图能加到 CAD：CadCraft 里能看到 ovimap 工程的标记、轨迹、标签与照片位置，
  并叠加底图瓦片作参照。
- 同步标签和轨迹：ovimap 工程 → CadCraft（MVP 主方向），CadCraft 改完 → ovimap
  （MVP 回传方向，见 §6 同步规则）。
- 滑洲云图与 CadCraft 通信：MVP 用文件交换 + CadCraft 控制通道 + ovimap 现有
  sync 协议桥接（§7）。不做实时双向同步（那是 Phase 2，§8）。

### 0.2 非目标（MVP 明确不做）

- 不做实时双向同步、不做多人协同编辑、不做冲突自动合并（§8 为设计，非实现）。
- 不做 KML / GPX 交换（CadCraft 侧 MVP 只做 GeoJSON + 带地理参照的 DXF）。
- 不做照片二进制传输（照片只传文件名 + 提示，见 §5.5）。
- 不做椭球横轴墨卡托（§3 用球面横轴墨卡托；椭球版为后续可选项，见 §3.7）。
- 不碰 ovimap 现有 DXF 出图链路（§3.6：现有 DXF 用等距圆柱近似，与本联动投影不同，
  两者不得混用）。

### 0.3 术语

| 术语 | 含义 |
|---|---|
| 工程 | ovimap 一个 collection（`collection_<cid>.json`），含一组 `MapLabel` |
| 标记 | ovimap 打点（杆、井、箱体等），`MapLabel`，非链顶点、非纯文字 |
| 轨迹 | 一条链（`buildLabelChains` 的一个 `lineGroupId` 分组），含若干顶点 |
| 标签 | 纯文字点（`typeId == 'text'`） |
| 地理参照 | 工程中心原点 + 投影参数（`ovimap_georef`），WGS84 ↔ CAD 平面米的桥梁 |
| CAD 平面坐标 | 本联动投影输出，单位米，见 §3 |

## 1. 坐标基准（datum）结论：ovimap 存储一律 WGS84

### 1.1 结论（查代码得出，不猜）

- **`MapLabel.lat/lon 一律 WGS84。`**`lib/models/map_label.dart` 开头注释原文：
  “地图上的一句话标签（通信基础设施点）。一律以 WGS-84 存储。”
- **`MapSourceEntry.datum` 不是存储基准，而是“显示换算”标记。**
  `lib/models/map_source.dart` 注释原文：“坐标基准（0=无转换 WGS84，1=GCJ-02，2=BD-09）”。
  `lib/geo/geo_convert.dart` 定义 `wgs84=0 / gcj02=1 / bd09=2`，并提供双向换算。
- **换算只发生在显示/点击边界，永不污染存储：**
  - 显示：存储 WGS84 → `GeoConvert.wgs84To(lat, lon, datum)` → 该图源坐标系下的点，
    与图源瓦片对齐（datum=1 的 sydt/高德瓦片是 GCJ-02 坐标系）。
  - 点击/打点：地图点击坐标（图源系）→ `GeoConvert.toWgs84(lat, lon, datum)` →
    转回 WGS84 再存入 `MapLabel`。
  - 检索：高德/天地图 POI 检索结果是 GCJ-02，`convertGcj=true` 时纠偏为 WGS84
    再入库（`lib/export/dxf.dart`、`lib/export/basemap.dart` 均有此参数）。
- **GeoJSON 一律 WGS84（RFC 7946 §4）。** 任何 GCJ-02 / BD-09 坐标必须在写出
  GeoJSON 之前用 `GeoConvert.toWgs84` 转回 WGS84；CadCraft 侧收到 GeoJSON
  一律按 WGS84 处理，绝不二次换算。反向（CadCraft → ovimap）同理：
  CadCraft 的 CAD 平面坐标 → §3 逆投影 → WGS84 → 直接存入 `MapLabel`，
  不经过 GCJ-02。

### 1.2 各图源 datum 对照（`map_source.dart` 实查）

| 图源 id | datum | 说明 |
|---|---|---|
| `sydt` / `sydt-s` / `sydt-m` / `sydt-p` | 1（GCJ-02） | 专有服务，CadCraft 侧**绝不**使用（§9 红线） |
| `amap-s` / `amap-ann` / `amap-road` | 1（GCJ-02） | 高德，专有，CadCraft 侧**绝不**使用（§9 红线） |
| `osm` / `osm-proxy` | 0（WGS84） | OSM，无偏移；`osm-proxy` 是用户自建反代，CadCraft 默认也不用它（§9） |
| `geovis-img` | 0（WGS84） | 星图地球影像，WGS84 无偏移 |
| `tdt-*`（天地图，需 key） | 0（WGS84） | CGCS2000 ≈ WGS84，代码里按 0 处理 |

### 1.3 GCJ-02 换算测试向量（用 `geo_convert.dart` 算法实算，非编造）

信阳市中心点：

| | 纬度 | 经度 |
|---|---|---|
| WGS84（存储值） | 32.1301 | 114.0814 |
| GCJ-02（`wgs84ToGcj02` 输出） | 32.1281536 | 114.0872867 |
| 偏移量 | −216.7 m（南） | +554.9 m（东），合计约 **595.7 m** |

与代码注释“信阳地区偏移约 300~600 米”一致。测试 T9 用此向量断言换算方向正确
（WGS84 → GCJ-02 向东南偏；反向则还原）。

### 1.4 转换关系总表

| 方向 | 方法 | 用途 |
|---|---|---|
| WGS84 → GCJ-02 | `GeoConvert.wgs84ToGcj02` | ovimap 内显示到 GCJ-02 图源时 |
| GCJ-02 → WGS84 | `GeoConvert.gcj02ToWgs84`（迭代反算，厘米级） | 检索结果入库、点击入库前 |
| WGS84 → BD-09 / 逆向 | `GeoConvert.wgs84ToBd09` / `bd09ToWgs84` | 仅百度系数据，联动不使用 |
| WGS84 ↔ CAD 米 | §3 球面横轴墨卡托正/逆算 | 本联动唯一投影通道 |
| GCJ-02 → GeoJSON | **禁止直写**，必须先 GCJ-02 → WGS84 | 写出前换算（§2） |

## 2. 交换格式：GeoJSON（WGS84）完整 Schema

### 2.1 文件与版本

- 文件名：`<工程名>.ovimap.geojson`（UTF-8，LF 换行）。
- 顶层必须是 `FeatureCollection`（RFC 7946）。坐标一律 `[经度, 纬度]`，
  2 个元素；若出现第 3 个元素（高程），导入方**必须忽略**（本联动是 2D）。
- 经纬度范围校验：经度 ∈ [−180, 180]，纬度 ∈ [−90, 90]；越界 → 整个文件拒绝导入
  （fail-closed，报错指明第一个越界要素）。
- Schema 版本：`ovimap_meta.schema_version = "1.0"`。导入方遇未知大版本（如 `2.0`）
  必须拒绝并提示升级；小版本未知字段按 §2.6 透传规则处理。

### 2.2 顶层结构（三个外来成员 + features）

`ovimap_georef` / `ovimap_meta` / `ovimap_deletes` 是 GeoJSON 外来成员
（RFC 7946 §7.1 允许，通用读取器会忽略它们），不是标准字段。

```json
{
  "type": "FeatureCollection",
  "ovimap_georef": {
    "projection": "OVIMAP_SPHERICAL_TM",
    "source_crs": "WGS84",
    "origin_lat": 32.1301,
    "origin_lon": 114.0814,
    "central_meridian": 114.0814,
    "scale_factor": 1.0,
    "false_easting": 0.0,
    "false_northing": 0.0,
    "radius": 6378137.0,
    "units": "metre"
  },
  "ovimap_meta": {
    "schema_version": "1.0",
    "exporter": "ovimap",
    "exported_at": "2026-10-14T05:00:00.000Z",
    "source_collection_id": "<ovimap collection id>",
    "source_collection_name": "<工程名>",
    "delete_mode": "none"
  },
  "ovimap_deletes": [],
  "features": [ "...见 §2.4 示例..." ]
}
```

| 字段 | 含义 | 规则 |
|---|---|---|
| `ovimap_georef` | 地理参照，见 §3 | 必填；导入方必须校验 `projection == "OVIMAP_SPHERICAL_TM"`，否则拒绝 |
| `ovimap_meta.exporter` | `"ovimap"` 或 `"cadcraft"` | 标明谁写出此文件 |
| `ovimap_meta.exported_at` | 写出时刻，ISO-8601 UTC | 供冲突判断参考（§6.4），不是要素级时间 |
| `ovimap_meta.source_collection_id/name` | ovimap 工程 id/名 | CadCraft 回传时原样带回 |
| `ovimap_meta.delete_mode` | `"none"`（默认）或 `"sync"` | 仅 `"sync"` 时 `ovimap_deletes` 生效（§6.3） |
| `ovimap_deletes` | `ovimap_id` 字符串数组 | 要删除的要素 id；默认不生效，必须配合 `delete_mode == "sync"` |

### 2.3 Feature properties 完整 Schema

#### 2.3.1 通用字段（四种 kind 共有）

| 属性 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `ovimap_id` | string | 是 | **稳定 ID，同步主键。** 标记/标签/照片 = `MapLabel.id`；轨迹 = `lineGroupId`（见 §2.5） |
| `name` | string | 是（可空串） | 显示名。标记：`MapLabel.name` 非空则用它，否则用类型中文名（如“水泥杆”）；标签：标签文本；轨迹：工程名或链名 |
| `note` | string | 否（缺省 `""`） | 备注，对应 `MapLabel.note` |
| `kind` | string | 是 | `marker` \| `track` \| `label` \| `photo`（§2.5 推导规则） |
| `type` | string | 是 | ovimap 类型 id：`pipe/concrete/wood/electric/manhole/handwell/fiberbox/splitterbox/onubox/crossbox/riser/room/bts/track/none/text/area`。CadCraft 原生要素无 ovimap 类型时，CadCraft 写出用 `"none"` |
| `symbol` | string | 否 | 单字符号（如 `"电"`），来自 `LabelType.symbol`；供 CadCraft 渲染参考 |
| `created_at` | string \| null | 否 | ISO-8601 UTC。来源见 §2.7；**仅供参考，不是同步主键** |
| `updated_at` | string \| null | 否 | ISO-8601 UTC。来源见 §2.7；MVP 为工程级时间（§6.4 说明粗糙性） |
| `photos` | string[] | 否（缺省 `[]`） | **仅文件名**（basename），如 `["IMG_001.jpg"]`。禁止路径、禁止 `..`、禁止 `/` `\`；违规条目丢弃并告警（§5.5） |
| `passthrough` | object | 否 | 未知属性透传袋（§2.6），导入方必须原样带回 |

#### 2.3.2 ovimap 原生可选字段（仅当值非默认时写出；导入方未知字段按 §2.6 透传）

| 属性 | 类型 | 对应 `MapLabel` 字段 | 写出条件 |
|---|---|---|---|
| `seq` | int | `seq` | 恒写（链内顺序的辅助，轨迹顶点也见 §2.5） |
| `line_group` | string | `lineGroupId` | 轨迹必填；标记/标签为空时省略 |
| `distanceM` | number | `distanceM`（本点到上一点的人工确认距离，米） | 非 null 时写出 |
| `distLabel` | string | `distLabel`（自定义标注，如“埋42.5”） | 非空时写出 |
| `segKind` | int | `segKind`（0默认/1架空/2埋地/3管道） | ≠0 时写出 |
| `segCable` | string | `segCable`（本段光缆型号，如“48芯GYTS”） | 非空时写出 |
| `slackM` | number | `slackM`（接头预留盘留，米） | >0 时写出 |
| `holes` / `usedHoles` | int | 人孔/手井总孔数/已占用 | >0 时写出 |
| `splitterRatio` | string | `splitterRatio`（如“1:8”） | 非空时写出 |
| `topoParentId` | string | `topoParentId`（拓扑上级节点 id） | 非空时写出 |
| `cableSpec` / `cableCores` | string / int | `cableSpec` / `cableCores` | 非空 / >0 时写出 |
| `reno` | int | `reno`（0原有/1新增/2拆除） | ≠0 时写出 |

> `MapLabel.extra`（如勘察表单 `extra['survey']`）**MVP 不进 GeoJSON**（避免
> 业务数据膨胀与隐私外泄）；如后续需要，走 `passthrough` 显式白名单扩展，
> 不得默认全量导出。

### 2.4 完整示例（标记 + 轨迹 + 标签 + 照片）

```json
{
  "type": "FeatureCollection",
  "ovimap_georef": {
    "projection": "OVIMAP_SPHERICAL_TM", "source_crs": "WGS84",
    "origin_lat": 32.1301, "origin_lon": 114.0814,
    "central_meridian": 114.0814, "scale_factor": 1.0,
    "false_easting": 0.0, "false_northing": 0.0,
    "radius": 6378137.0, "units": "metre"
  },
  "ovimap_meta": {
    "schema_version": "1.0", "exporter": "ovimap",
    "exported_at": "2026-10-14T05:00:00.000Z",
    "source_collection_id": "c-001", "source_collection_name": "测试工程",
    "delete_mode": "none"
  },
  "ovimap_deletes": [],
  "features": [
    {
      "type": "Feature",
      "geometry": {"type": "Point", "coordinates": [114.0814000, 32.1301000]},
      "properties": {
        "ovimap_id": "11111111-1111-4111-8111-111111111111",
        "name": "1号杆", "note": "路口东侧", "kind": "marker",
        "type": "concrete", "symbol": "电", "seq": 1,
        "photos": ["IMG_001.jpg"],
        "created_at": "2026-10-13T08:00:00.000Z", "updated_at": "2026-10-14T05:00:00.000Z"
      }
    },
    {
      "type": "Feature",
      "geometry": {
        "type": "LineString",
        "coordinates": [
          [114.0814000, 32.1301000],
          [114.0824000, 32.1305000],
          [114.0834000, 32.1302000]
        ]
      },
      "properties": {
        "ovimap_id": "line-001",
        "name": "杆路1", "note": "", "kind": "track", "type": "track",
        "line_group": "line-001", "closed": false,
        "timestamps": null,
        "vertices": [
          {"ovimap_id": "11111111-1111-4111-8111-111111111111",
           "name": "1号杆", "type": "concrete", "note": "路口东侧",
           "seq": 1, "photos": ["IMG_001.jpg"]},
          {"ovimap_id": "22222222-2222-4222-8222-222222222222",
           "name": "2号杆", "type": "concrete", "note": "",
           "seq": 2, "distanceM": 105.5, "segKind": 1, "segCable": "48芯GYTS"},
          {"ovimap_id": "33333333-3333-4333-8333-333333333333",
           "name": "3号杆", "type": "concrete", "note": "",
           "seq": 3, "distanceM": 98.0}
        ],
        "created_at": "2026-10-13T08:00:00.000Z", "updated_at": "2026-10-14T05:00:00.000Z"
      }
    },
    {
      "type": "Feature",
      "geometry": {"type": "Point", "coordinates": [114.0820000, 32.1310000]},
      "properties": {
        "ovimap_id": "44444444-4444-4444-8444-444444444444",
        "name": "此处需埋地", "note": "", "kind": "label", "type": "text"
      }
    },
    {
      "type": "Feature",
      "geometry": {"type": "Point", "coordinates": [114.0819000, 32.1308000]},
      "properties": {
        "ovimap_id": "55555555-5555-4555-8555-555555555555",
        "name": "现场照片", "note": "箱体取证", "kind": "photo",
        "type": "none", "photos": ["IMG_009.jpg"]
      }
    }
  ]
}
```

### 2.5 kind 推导与轨迹顶点规则

#### 2.5.1 kind 推导（ovimap → GeoJSON）

| ovimap 输入 | GeoJSON kind | 说明 |
|---|---|---|
| `typeId == 'text'` | `label` | 纯文字点，Point 几何 |
| 链成员（`buildLabelChains` 输出的链内点，`isLineMember` 为真） | 不单独成要素 | 只作为轨迹的顶点（见下） |
| 非链成员、非文字点 | `marker` | Point 几何；独立个体（箱体/机房/引上等）天然在此列 |
| `extra['ovimap_link']['kind'] == 'photo'` 的点 | `photo` | 仅用于 CadCraft 回传的照片钉（§2.5.3）；ovimap 原生打点**永不**导出为 `photo`，照片挂在 `photos[]` 上 |

#### 2.5.2 轨迹（kind=track）

- 一条链（`buildLabelChains` 的一个分组）= 一个 `track` Feature，
  几何为 `LineString`，坐标按链顺序排列。
- 轨迹 Feature 的 `ovimap_id` = 该链的 `lineGroupId`（链本身没有独立 id，
  用组 id 作稳定标识；导入方必须校验：同一文件内轨迹 `ovimap_id` 不得与任何
  标记/标签/照片的 `ovimap_id` 重合，重合 → 拒绝整个文件）。
- `line_group` 必填 = `lineGroupId`；`closed`：链首点 `closedArea` 为真时为
  `true`，此时坐标首尾重复（闭合环），CadCraft 映射为闭合 LWPOLYLINE（§5.2）。
- `vertices[]`：与坐标**一一对齐**的对象数组，承载每个顶点的身份与业务字段
  （`ovimap_id`、`name`、`type`、`note`、`seq`、`distanceM`、`distLabel`、
  `segKind`、`segCable`、`slackM`、`photos[]` 等，缺省字段省略）。
  `vertices` 长度必须等于坐标数，否则拒绝文件。
- `timestamps`：与坐标一一对齐的 ISO-8601 UTC 字符串数组，或 `null`。
  **`MapLabel` 没有逐点时间戳字段，ovimap 导出必须写 `"timestamps": null`，
  绝不编造时间。** 导入方：`timestamps` 为数组时长度必须等于坐标数，否则拒绝；
  为 `null` 时接受。未来 ovimap 增加逐点时间戳后，再写出真实数组（Schema 不变）。

#### 2.5.3 照片（kind=photo）与 `photos[]`

- ovimap 原生：照片挂在点上（`MapLabel.photoPaths`），导出为所属要素的
  `photos[]`（标记的 `photos[]`、轨迹顶点的 `photos[]`）。
- `kind=photo` 仅用于 CadCraft 原生照片钉（用户在 CAD 里钉一个照片位置，
  只含文件名）。ovimap 导入 `kind=photo` 时：生成 `MapLabel`，
  `typeId` 取 `ovimap_type`（若有）否则 `'text'`（可见的独立标签），
  `photos` 原样保留，并在 `extra['ovimap_link']['kind'] = 'photo'` 记录，
  以便再导出时恢复 `kind=photo`（往返不丢 kind）。
- `photos[]` 元素只允许 basename；含 `/`、`\`、`..` 的条目 → 丢弃该条目并记入
  导入报告，**不**拒绝整个文件（照片是附属数据，见 §5.5）。

### 2.6 透传规则（未知属性不得丢）

- 导入方遇到不认识的 properties 字段（非 §2.3 表内），**必须**原样保存在
  透传袋中，再导出时原样写回：ovimap 侧存入
  `extra['ovimap_link']['passthrough']`（按字段名键值保存）；轨迹顶点未知字段
  存入 `vertices[i].passthrough`；CadCraft 侧存入要素的私有扩展区。
- CadCraft 可写 `cad_*` 前缀属性（如 `cad_color`）；ovimap 必须透传，不得删除。
- 例外：`passthrough` 内不得出现 token、密钥、绝对路径；导入方发现此类值
  必须丢弃并告警（文件里不该有敏感信息，§7.4）。

### 2.7 `created_at` / `updated_at` 来源（不编造）

- `MapLabel` **没有**逐点创建时间/更新时间字段。ovimap 导出时：
  全部要素的 `created_at` 取工程的 `createdAt`（`collection_<cid>.json` /
  `index.json` 的 `createdAt`，毫秒时间戳 → ISO-8601 UTC）；缺失则写 `null`。
  `updated_at` 取工程同步元数据 `updatedAt`（`sync_state.json` / `_sync`），
  缺失则写 `null`。**绝不给每个要素编不同的假时间。**
- CadCraft 原生要素的 `created_at`/`updated_at` 由 CadCraft 真实记录；
  ovimap 导入时保存在 `extra['ovimap_link']['created_at'/'updated_at']`，
  再导出时优先用保存值（保证 CadCraft 原生要素往返时间稳定）。
- 这两个字段仅供参考/冲突判断辅助，**同步主键永远是 `ovimap_id`**（§6）。

### 2.8 坐标精度

- 写出：经纬度至少 9 位小数（约 0.1mm），推荐写出双精度最短往返表示
  （如 Dart `toString()` / Rust `ryu`），禁止用 7 位小数（约 1.1cm，会丢精度）。
- 导入方不得对坐标做二次取整；往返测试容差见 §10（GeoJSON ≤1e-9°，DXF ≤1mm）。

## 3. 投影：球面横轴墨卡托（OVIMAP_SPHERICAL_TM）

### 3.1 参数（固定，不可协商）

| 参数 | 值 |
|---|---|
| 投影 | 横轴墨卡托（Transverse Mercator），**球面**公式 |
| 球半径 R | **6378137** m（WGS84 长半轴数值，但按球面使用） |
| 中央子午线 λ₀ | 工程中心经度（`ovimap_georef.central_meridian`） |
| 原点纬度 φ₀ | 工程中心纬度（`ovimap_georef.origin_lat`） |
| 原点经度 | 工程中心经度（`ovimap_georef.origin_lon`，与中央子午线相同） |
| 比例因子 k₀ | **1** |
| false easting / false northing | **0 / 0** |
| 输出 | 米（x 向东，y 向北；工程中心为 (0, 0)） |
| 输入 | WGS84 经纬度（度 → 转弧度计算） |

- 原点 = 工程中心：取工程全部要素经纬度的中心
  （`((minLat+maxLat)/2, (minLon+maxLon)/2)`）；单点工程则取该点。
  每次导出按当前工程范围重算原点，并写进 `ovimap_georef`；导入方**必须**
  以文件内的 `ovimap_georef` 为准，不得用自己缓存的旧原点。
- 工程范围变化导致原点变化时，同一工程的两次导出 CAD 坐标会整体平移
  （这是设计行为，不是 bug）；往返时以各自文件的 `ovimap_georef` 为准即可还原。

### 3.2 正算（WGS84 → CAD 米）

设 φ、λ 为弧度，Δλ = λ − λ₀，φ₀ 为原点纬度（弧度）：

```text
B = cos(φ) · sin(Δλ)
x = R · atanh(B)                                  // atanh(B) = 0.5·ln((1+B)/(1−B))
y = R · ( atan2(sin(φ), cos(φ)·cos(Δλ)) − φ₀ )
```

### 3.3 逆算（CAD 米 → WGS84）

```text
φ' = y / R + φ₀
B  = tanh(x / R)
D  = sqrt(1 − B²)
φ  = asin( sin(φ') · D )
Δλ = atan2( B, cos(φ') · D )
λ  = λ₀ + Δλ
```

正/逆算使用**同一组**公式；往返误差仅为浮点舍入（实测 < 1e-6 mm，见 §3.5）。

### 3.4 为什么不用 Web Mercator（EPSG:3857）

Web Mercator 在纬度 φ 处的点比例因子为 `k = 1/cos(φ)`。信阳（φ≈32.13°）
处 `k ≈ 1.179`，即**尺度系统性放大约 17.9%**：50 km 的工程在 CAD 里会被画成
约 59 km，且越往南北两端变形越大。工程图要求距离可量测，Web Mercator
不可用（仅适合做全球小比例尺浏览底图）。本联动的球面 TM 在原点处 k₀=1，
变形随离原点距离增长而缓慢增大（§3.5），是工程可接受的量级。

### 3.5 实测误差：球面 TM vs WGS84 椭球 TM（50 km 内，数值实算）

> 方法（可复现，禁止编造）：原点 (32.1301, 114.0814)；采样点按本地 ENU
> 在各半径圆上以 1° 方位角步进布设（用原点处子午圈曲率半径 M₀ 与卯酉圈
> 曲率半径 N₀换算）；椭球 TM 用 Snyder 六阶级数公式（WGS84 a=6378137，
> f=1/298.257223563，中央子午线 0.5° 内精度约毫米级）；球面 TM 用 §3.2
> 闭式；误差 = 两投影坐标的平面距离。Web Mercator 对照用
> x=R·Δλ、y=R·(ln tan(π/4+φ/2) − ln tan(π/4+φ₀/2))。
> 脚本：`python3` 标准库实现（采样 + 两投影 + 对照），结果如下。

| 离原点距离 | 球面 vs 椭球 TM 最大误差 | 出现方位 | Web Mercator vs 椭球 TM 最大误差 |
|---|---|---|---|
| 1 km | **3.88 m** | 正南 | — |
| 2 km | **7.77 m** | 正南 | — |
| 5 km | **19.42 m** | 正南 | — |
| 10 km | **38.89 m** | 正南 | — |
| 20 km | **77.91 m** | 正南 | — |
| 30 km | **117.08 m** | 正南 | — |
| 40 km | **156.39 m** | 正南 | — |
| **50 km** | **195.85 m** | 正南（dx=0，dy=−195.85） | **9417.80 m**（正北） |

50 km 八方位明细：

| 方位 | 球面 vs 椭球误差 | Web Mercator vs 椭球误差 |
|---|---|---|
| 正北 | 192.28 m | 9417.80 m |
| 正南 | **195.85 m（50 km 内最大值）** | 9128.41 m |
| 正东 / 正西 | 47.36 m | 8987.45 m |
| 东北 / 西北 | 140.44 m | 9224.67 m |
| 东南 / 西南 | 142.06 m | 9037.89 m |

成因（可验算）：球面 TM 用 R=6378137 代替椭球曲率半径。原点处子午圈曲率
半径 M₀=6353476.8 m（R−M₀=24660.2 m，相对 +0.3881%），卯酉圈曲率半径
N₀=6384184.3 m（R−N₀=−6047.3 m，相对 −0.0947%）。南北向误差 ≈ 距离×0.39%，
东西向 ≈ 距离×0.095%，与上表一致（50 km 南：50000×0.39%≈195 m）。

**结论与使用边界（必须写进实现与测试）：**

1. 这 195.85 m 是**球面模型 vs WGS84 椭球真值**的模型偏差，不是往返丢失：
   同一套球面公式正/逆算往返误差实测 < 1e-6 mm（浮点舍入级），GeoJSON →
   CAD → GeoJSON 不丢坐标（测试 T2/T3 断言）。
2. CAD 平面坐标是**局部工程坐标**，不是大地测量坐标。不得把 CAD 的 x/y
   当作 UTM / 高斯-克吕格坐标使用，不得与第三方测量成果直接拼图。
3. 误差随离原点距离**线性**增长：典型杆路工程（半径 ≤2 km）最大约 7.8 m，
   仍大于杆位容差——因此 CAD 里的距离**只能作示意**，竣工量测以 ovimap
   原始 WGS84 + 大地线距离为准；CAD 不作为结算依据。
4. 原点取工程中心（§3.1），使误差从中心向外增长；不得用固定城市中心作全市
   工程的原点（会放大边缘误差）。
5. 若未来工程要求测量级精度，升级路径见 §3.7（换椭球 TM，`projection` 名变更，
   旧文件仍按旧名解析，互不干扰）。

### 3.6 与 ovimap 现有 DXF 出图的投影区分（重要，不得混淆）

- ovimap 现有 DXF 导出（`lib/export/dxf.dart:198-205`）用**等距圆柱近似**：
  `scaleX = 111320·cos(baseLat)`，`scaleY = 110540`，`x=(lon−baseLon)·scaleX`，
  `y=(lat−baseLat)·scaleY`。那是出图用的纸面坐标换算，**不是**本联动投影。
- **联动 DXF（§4）必须用本节球面 TM**，不得复用等距圆柱近似。同一工程不得
  混用两种投影；如未来统一，以本节为准。
- 现有 DXF 出图不含地理参照（无 APPID/XRECORD，见 §4.1），与联动 DXF 是两个
  不同的导出路径（菜单/函数分开，不得互相覆盖）。

### 3.7 后续可选项（不在 MVP 内，仅预留）

- 椭球横轴墨卡托（WGS84 椭球，Snyder 公式）：若某工程要求测量级精度，可新增
  `projection = "OVIMAP_ELLIPSOIDAL_TM"`，其余字段不变；导入方按 `projection`
  名分发。MVP 实现**只需**支持球面版，遇椭球版文件必须拒绝并提示（不得按球面
  公式误算）。
- 不做 UTM（分带、500 km false easting）——工程局部坐标不需要分带。

## 4. DXF 地理参照

### 4.1 现状（查代码得出）

- ovimap 现有 DXF 写出器（`lib/export/dxf.dart`）已写 TABLES 段（LAYER + STYLE）
  与 OBJECTS 段（根命名对象字典 + ACAD_GROUP），**未写 APPID 表、未写任何
  XRECORD、实体无 xdata**。联动需要新增这三部分（实现任务，非本文档）。
- CadCraft 已有实体 xdata（`Common.xdata`）与外来对象/XRECORD 透传
  （`crates/doc/src/foreign.rs`：`ForeignObject` / `xdata` 跟随实体复制与保存，
  见 `crates/engine/tests/xdata_follows_entity.rs`）。联动实现必须验证：
  CadCraft 打开并另存带 OVIMAP XRECORD 的 DXF 后，XRECORD 仍在（测试 T3）。
  若验证失败，DXF 路径按 §4.6 降级（轨迹顶点丢 ID 并告警）。

### 4.2 总体结构（DXF R2000 / AC1015 及以上）

```
HEADER 段：$INSUNITS = 6（米，见 §4.5）
TABLES 段：APPID 表必须含 OVIMAP 条目（§4.3），且在任何 xdata 使用之前
ENTITIES 段：POINT / TEXT / MTEXT / LWPOLYLINE，带 xdata（§4.4）
OBJECTS 段：根命名对象字典 → OVIMAP_GEOREF 字典 → GEOREF XRECORD + VERTICES_* XRECORD（§4.3）
```

- 句柄（group 5）由写出器分配唯一值；读取方**必须按名字解析**（`OVIMAP` /
  `OVIMAP_GEOREF` / `GEOREF` / `VERTICES_<id>`），不得硬编码句柄值。
- CadCraft 自有扩展用 `CADCRAFT` APPID（`crates/io/src/dxf_ext.rs`），与
  `OVIMAP` 互不干扰；同一实体可同时带两套 xdata。

### 4.3 APPID + DICTIONARY + XRECORD（精确组码）

#### APPID 表（TABLES 段）

```
0 APPID
2 OVIMAP
70 0
```

#### OBJECTS 段：根命名对象字典 → OVIMAP_GEOREF 字典

```
0 DICTIONARY            ; 根命名对象字典（ACAD_GROUP 旁，已有）
3 OVIMAP_GEOREF
350 <OVIMAP_GEOREF 字典句柄>

0 DICTIONARY            ; OVIMAP_GEOREF 字典
5 <句柄>
330 <根字典句柄>
100 AcDbDictionary
280 1
281 1
3 GEOREF
350 <GEOREF XRECORD 句柄>
3 VERTICES_<lineGroupId>      ; 每个轨迹一条（§4.4）
350 <该轨迹 VERTICES XRECORD 句柄>
```

#### GEOREF XRECORD（组码顺序固定，读取方按顺序解析）

| 组码 | 值 | 说明 |
|---|---|---|
| 5 | 句柄 | 写出器分配 |
| 330 | 所有者句柄 | OVIMAP_GEOREF 字典句柄 |
| 100 | `AcDbXrecord` | 子类标记 |
| 280 | 1 | 克隆标志 |
| 1000 | `OVIMAP_SPHERICAL_TM` | 投影名（与 GeoJSON `ovimap_georef.projection` 一致） |
| 1040 | origin_lat（度） | 原点纬度 |
| 1040 | origin_lon（度） | 原点经度 |
| 1040 | central_meridian（度） | 中央子午线（= origin_lon） |
| 1040 | scale_factor（1.0） | k₀ |
| 1040 | radius（6378137.0） | R |
| 1000 | `metre` | 单位 |
| 1000 | `WGS84` | 源 CRS |
| 1000 | `1.0` | Schema 版本（与 `ovimap_meta.schema_version` 一致） |

读取方校验：投影名、k₀、R 与 §3 不一致 → 拒绝导入并报错。

#### VERTICES_\<lineGroupId\> XRECORD（每个轨迹一条）

DXF 的 LWPOLYLINE 顶点没有 xdata（DXF 限制），轨迹顶点的 `ovimap_id`
只能放在这里。这是 DXF 路径往返不丢 ID 的关键。

| 组码 | 值 |
|---|---|
| 5 / 330 / 100 / 280 | 同上（XRECORD 通用头） |
| 1070 | 顶点数 N |
| 1000 × N | 顶点 `ovimap_id`，按 LWPOLYLINE 顶点顺序排列 |

- 字典键 `VERTICES_<lineGroupId>`：`lineGroupId` 原样拼接；若写出器的 DXF
  字典键不接受其中某个字符，**必须报错失败导出**，不得静默改名（改名会破坏
  键 ↔ 轨迹的连接，导入时无法还原顶点 ID）。
- 导入时：若某轨迹无对应的 VERTICES XRECORD（如文件经第三方软件转存被剥离），
  按 §4.6 降级：顶点分配新 ID，导入报告中警告，并列出受影响轨迹。

### 4.4 实体 xdata（每个联动实体都带）

组码顺序固定：

```
1001 OVIMAP
1000 <ovimap_id>
1000 <name>
1000 <kind>
1000 <type>
```

- `1001` 必须为 `OVIMAP`（注册应用名，与 APPID 表条目一致；DXF 要求 xdata
  使用前 APPID 已注册）。
- xdata 字符串（1000）单条最长 255 字符（DXF 规范）：超长 `name` 写出时截断
  并告警，完整名以 GeoJSON 为准（GeoJSON 是规范路径，DXF 是兼容路径）。
- 标记的 POINT 与 TEXT 带**同一份** xdata（同一 `ovimap_id`）；照片提示 TEXT
  同样带所属要素的 xdata（§5.5）。

### 4.5 单位与图层

- HEADER：`$INSUNITS = 6`（米）。导入方要求：`$INSUNITS` 为 6 或缺省（缺省时
  导入方置 6）；为其他值 → 拒绝导入并报错（不得按错误单位误算）。
- 图层（导入时不存在则创建；**永不**删除用户已有图层）：

| 图层 | 用途 | 推荐颜色（仅显示，非语义） |
|---|---|---|
| `OVIMAP_MARKER` | 标记 POINT + TEXT | 7（白） |
| `OVIMAP_TRACK` | 轨迹 LWPOLYLINE | 3（绿） |
| `OVIMAP_LABEL` | 标签 TEXT / MTEXT | 2（黄） |
| `OVIMAP_PHOTO` | 照片提示 TEXT | 6（品红） |

颜色仅为显示建议，不参与同步判断；导入方不得因颜色不同而拒绝或改写。

### 4.6 DXF 路径的精度与降级（必须如实告知实现方与用户）

| 项目 | GeoJSON 路径（规范） | DXF 路径（兼容） |
|---|---|---|
| 坐标精度 | 往返 ≤1e-9°（约 0.1mm） | DXF 双精度，往返 ≤1mm |
| 轨迹顶点 ID | `vertices[]` 完整保留 | 依赖 VERTICES_* XRECORD；被剥离则顶点重编新 ID 并告警 |
| 业务字段（segCable 等） | 完整保留（§2.3.2） | **不保留**（DXF 实体无处存放；XRECORD 只放顶点 ID） |
| 照片 | `photos[]` 完整 | 仅提示 TEXT（§5.5），文件名仍保留 |
| 透传属性 | 完整（§2.6） | **不保留** |
| `created_at` / `updated_at` | 保留 | 不保留 |

**结论：** 需要完整往返时用 GeoJSON；DXF 用于在 CAD 里查看/量测/打印，以及
把 CAD 里改过的几何带回 ovimap（几何 + ID 保留，业务字段以 ovimap 侧原值为准，
DXF 侧不覆盖业务字段）。

## 5. 映射表：ovimap ↔ CAD

### 5.1 方向总则

- **几何权威：** POINT / LWPOLYLINE 的几何是权威；TEXT 位置是派生的装饰
  （导入方不得用 TEXT 位置反推要素坐标；用户在 CAD 里只拖动 TEXT 不改变
  同步内容，见 §6.5）。
- **ID 权威：** xdata / `ovimap_id` 是身份；CAD 句柄（handle）不是身份，
  每次保存都可能变化，绝不以句柄作同步键。

### 5.2 ovimap → CAD（导出映射）

| ovimap 输入 | CAD 输出 | 图层 | xdata |
|---|---|---|---|
| 标记（非链成员、非文字点；`typeId` 任意非 `text`） | POINT（§3 正算 XY）+ TEXT（内容 = `name`，若空则 `symbol`；字高 2.5 米，位于 POINT 上方，偏移量由 CadCraft 显示层决定，不进同步） | `OVIMAP_MARKER` | 两实体同一份（§4.4） |
| 轨迹链（`buildLabelChains` 一组） | LWPOLYLINE（顶点 = §3 正算 XY，按链顺序；`closedArea` 为真时闭合标志 70 bit 1 且首尾点重复） | `OVIMAP_TRACK` | `ovimap_id` = `lineGroupId`，`kind` = `track`，`type` = `track`；顶点 ID 进 VERTICES_* XRECORD（DXF）/ `vertices[]`（GeoJSON） |
| 标签（`typeId == 'text'`） | TEXT（内容 = 标签文本；含换行时用 MTEXT） | `OVIMAP_LABEL` | §4.4（`kind` = `label`） |
| 照片钉（`kind == 'photo'`，仅 CadCraft 原生） | POINT + TEXT（内容 = `PHOTO: <basename>`） | `OVIMAP_PHOTO` | §4.4（`kind` = `photo`） |
| 标记/顶点附带的照片 | 附加 TEXT（内容 = `PHOTO: <basename1>, <basename2>`，位于要素下方） | `OVIMAP_PHOTO` | 所属要素的同一份 xdata |

- TEXT 字高：2.5 米（绘图单位 = 米，§4.5）。注：50 km 工程下 2.5 米字在全图
  缩放时看不见是**预期行为**（真实尺寸），用户缩放至要素附近即可见；不得为
  “好看”而改用纸面缩放字高（那会破坏“CAD 坐标可量测”）。
- 独立个体（箱体/分光器箱/分纤盒/ONU 箱/机房/基站/引上）在 ovimap 里不参与
  连线（`isStandalone`），导出时只成标记，不进轨迹顶点（与地图行为一致）。

### 5.3 CAD → ovimap（导入映射）

| CAD 输入 | ovimap 输出 | 规则 |
|---|---|---|
| POINT + TEXT 配对（同一 `ovimap_id`，同在 `OVIMAP_MARKER`） | 一个 `MapLabel` | 坐标 = POINT 经 §3 逆算；`name`/`note`/`type` 取 xdata 与 TEXT 内容（TEXT 内容优先于 xdata 的 `name`，因用户可能在 CAD 里改了文字；改动计入同步，见 §6.2） |
| 无配对 POINT（有 OVIMAP xdata） | 一个 `MapLabel` | `name` 取 xdata |
| 无配对 TEXT（有 OVIMAP xdata，`kind` = `marker`） | 一个 `MapLabel`（`typeId = 'text'`） | 告警 + 记入导入报告；原 `kind` 存入 `extra['ovimap_link']['kind']`，往返不丢 |
| LWPOLYLINE（`OVIMAP_TRACK`，有 xdata） | 一条链（`lineGroupId` = xdata `ovimap_id`）+ 顶点 `MapLabel` | 顶点 ID：GeoJSON 取 `vertices[]`；DXF 取 VERTICES_* XRECORD；缺失则新编 ID 并告警（§4.6）。闭合标志 → 首点 `closedArea` |
| TEXT / MTEXT（`OVIMAP_LABEL`，`kind` = `label`） | `MapLabel`（`typeId = 'text'`） | 内容 → `name` |
| POINT + TEXT（`OVIMAP_PHOTO`，`kind` = `photo`） | `MapLabel`（`typeId` 取 xdata `type`，否则 `'text'`）+ `photos[]` + `extra['ovimap_link']['kind']='photo'` | §2.5.3 |
| 无 OVIMAP xdata 的实体 | **忽略**（不导入） | 用户在 CAD 里手画的辅助线不是 ovimap 数据；如需导入，用户须先用 CadCraft 命令赋予 OVIMAP 身份（实现方提供，见附录 C5） |

### 5.4 CadCraft 原生要素 → ovimap（新增要素）

CadCraft 用户新建的 POINT / LWPOLYLINE / TEXT（无 OVIMAP xdata）默认**不进**
ovimap 同步。用户显式执行“赋予 ovimap 身份”后，CadCraft 分配新 UUID 作
`ovimap_id`（v4 格式，与 `MapLabel._uuid` 一致），写 xdata，之后按 §5.3 导入。
此举避免把 CAD 临时辅助图形误同步到 ovimap。

### 5.5 照片规则

- 照片**只传文件名**，不嵌入二进制，不随 GeoJSON/DXF 走。
- 照片文件本身的流转（MVP）：人工拷贝，或 ovimap 现有同步照片通道
  （分块上传端点，见 §7.3；CadCraft MVP 不必实现照片上传）。
- CadCraft 导入时：按 `photos[]` 在图纸所在目录（及 `photos/` 子目录）查找
  文件；找不到 → **不失败**，在导入报告列出缺失照片清单，并对该要素显示
  `PHOTO: <basename>（文件缺失）` 提示。找到 → 可按文件名打开查看（实现方提供）。
- ovimap 导入时：`photos[]` 直接写入 `MapLabel.photoPaths`（仅 basename）；
  文件本身需用户另行拷贝到 ovimap `photos/` 目录，否则 ovimap 侧显示缺失提示。
- 导入报告必须包含：要素计数、缺失照片清单、丢弃的非法照片条目（§2.5.3）。

## 6. 同步规则（upsert / 删除 / 往返 / 冲突）

### 6.1 身份与 upsert

- 同步主键：`ovimap_id`（+ `kind` 校验：同一 `ovimap_id` 在两侧的 `kind`
  必须一致，不一致 → 拒绝该要素并记入报告，不得静默改 kind）。
- 导入 = 按 `ovimap_id` upsert：
  - id 不存在 → **新增**要素。
  - id 存在且内容变化（按 §6.2 判定）→ **更新**要素。
  - id 存在且内容相同 → **跳过**（0 改动）。
- 内容比较：比较规范化 JSON（键排序、坐标全精度），忽略 `created_at` /
  `updated_at` / `ovimap_meta` / `ovimap_georef`（后两者是文件级元数据，
  不参与要素比较）。
- `ovimap_id` 冲突（同一文件内轨迹 id 与标记 id 重合）→ 拒绝整个文件（§2.5.2）。

### 6.2 什么算“变化”（CadCraft → ovimap）

- 几何变化（POINT 移动、LWPOLYLINE 顶点移动/增删）→ 更新。
- `name` / `note` / `type` / `photos[]` 变化 → 更新。
- 业务字段（§2.3.2）变化 → 更新（GeoJSON 路径；DXF 路径不携带业务字段，
  见 §4.6，DXF 导入**不得**用缺省值覆盖 ovimap 侧已有业务字段）。
- TEXT 位置/字高/颜色/图层变化 → **不是变化**（装饰属性，忽略）。

### 6.3 删除（默认关闭，必须显式开启）

- **默认：导入永不因“对方文件里没有某 id”而删除。** 缺席 = 不在此次导出范围内，
  不是删除信号（导出可能只是部分要素）。
- 删除只通过 `ovimap_deletes` + `delete_mode == "sync"`（§2.2）生效：
  导入方删除列表中的要素，并记入报告。
- MVP 下 CadCraft 导出**不自动**生成 `ovimap_deletes`（CadCraft 不自动推断
  “用户删了什么”）；删除需用户显式操作（实现方提供“标记同步删除”命令，
  将 id 记入下一次导出的 `ovimap_deletes`），或手动编辑文件。
  Phase 2 将删除作为一等操作（§8）。

### 6.4 冲突（MVP：ovimap 赢，报告列出）

- MVP 冲突粒度是**工程级**（不是要素级），因为 `updated_at` 是工程级时间
  （§2.7）：若导入文件的 `exported_at` ≤ ovimap 侧上次同步戳且要素内容不同，
  判定为“双方都改过”→ **保留 ovimap 版本**，冲突要素 id 列入导入报告，
  CadCraft 侧改动不应用，用户手动重做。`exported_at` 更新 → 正常 upsert。
- 禁止静默覆盖、禁止自动合并几何（无合并 UI 的 MVP 下，合并必然丢数据）。

### 6.5 往返不变性（必须测试，见 §10）

- 同一文件重复导入 → 0 改动（幂等）。
- ovimap → GeoJSON → ovimap：要素数、id、坐标（≤1e-9°）、业务字段全部一致；
  CadCraft → GeoJSON → CadCraft：实体数、id、坐标（≤1mm）一致。
- ovimap → DXF → ovimap：要素数、id、几何（≤1mm）一致；业务字段以 ovimap
  原值为准（DXF 不覆盖）；轨迹顶点 ID 依赖 VERTICES_* XRECORD（§4.3），
  缺失时顶点重编 ID 并告警（§4.6）。

## 7. 通信：MVP 文件 + CadCraft 控制通道 + ovimap sync 桥接

### 7.1 文件交换（MVP 主通道，无需网络）

| 步骤 | 动作 | 接口（已存在 / 新增） |
|---|---|---|
| 1 | ovimap 导出 GeoJSON（+ 可选 DXF） | ovimap 新增导出：写 `<工程名>.ovimap.geojson` 到 `LabelStore.exportDir()`（现有导出目录机制，`lib/services/store.dart`），复用既有分享/保存流程 |
| 2 | 文件传到电脑 | 人工（USB / 微信文件传输 / 网盘），或 §7.3 桥接 |
| 3 | CadCraft 打开 | DXF：现有 `File > Open`；GeoJSON：新增 `File > Import > OviMap GeoJSON`（实现任务） |
| 4 | CadCraft 回传 | CadCraft 新增导出：写同名 `.ovimap.geojson` / `.ovimap.dxf` |
| 5 | ovimap 导入 | ovimap 新增导入：读 GeoJSON/DXF，按 §6 upsert，输出导入报告 |

- 文件里**绝不**含 token、密钥、绝对路径（§2.6 例外条款）；坐标是工程数据，
  通过可信渠道传输（§7.4）。

### 7.2 CadCraft 控制通道（MVP 自动化通道，复用现有协议）

CadCraft 已有控制协议与 MCP（`docs/control-protocol.md` / `docs/mcp.md`），
联动新增的命令挂在现有协议上，**不另起端口与协议**。

- 启动：`cadcraft --control PORT`（或 `CADCRAFT_CONTROL_PORT=PORT`），
  监听 `127.0.0.1:PORT`，JSON Lines（一行一请求、一行一回复）。
  MCP：`cadcraft-cli mcp --connect 127.0.0.1:PORT`。
- 新增命令（提议的命令 id，实现方注册后以 `engine.commands` 返回为准；
  未注册时调用方必须报“命令不存在”而非猜测别名）：

| 命令（提议 id） | 参数 | 作用 |
|---|---|---|
| `ovimap.import` | `{path}` | 导入 `.ovimap.geojson` 或带地理参照的 `.dxf`；返回导入报告（新增/更新/跳过/冲突/缺失照片计数） |
| `ovimap.export` | `{path, format: "geojson" \| "dxf"}` | 导出当前图纸的联动文件到 `path`；返回写出报告 |
| `ovimap.inspect` | `{}` | 返回当前图纸的联动状态：`ovimap_georef`、各 kind 计数、缺失照片清单 |

- 调用方式（控制协议，JSON Lines）：

```json
{"id": 1, "method": "engine.execute", "params": {"command": "ovimap.import", "params": {"path": "/path/to/工程.ovimap.geojson"}}}
{"id": 2, "method": "engine.execute", "params": {"command": "ovimap.export", "params": {"path": "/path/to/工程.ovimap.geojson", "format": "geojson"}}}
{"id": 3, "method": "drawing.inspect", "params": {}}
```

- 调用方式（MCP）：`execute {command: "ovimap.import", params: {path}}`，
  随后 `inspect_drawing {}` 核对实体数。
- 复用已有命令（不得重复造）：`app.open {path}`（打开 DXF）、`drawing.inspect`、
  `engine.execute {command: "document.bytes", params: {}}`（取回 DXF 字节，base64）。
- 注意：控制通道没有“下载文件”原语；`ovimap.export` 写到磁盘路径后，调用方
  自行读文件。GeoJSON 不走 base64 回传（MVP 不扩展协议）。

### 7.3 ovimap sync 桥接（可选，Phase 1.5；复用现有协议，不改现有端点语义）

ovimap 已有同步体系（Windows 内嵌 shelf 服务端 + SyncApi 客户端），
见 `lib/sync/sync_api.dart`（`GET /ping`、`GET /index`、`GET/PUT /project/:id`、
`DELETE /project/:id`、`GET /project/:id/history`、`POST /project/:id/restore`，
统一信封 `{code, data, message}`，`Authorization: Bearer <token>` +
`X-Device-Id` + `X-Device-Name`，PUT/DELETE 带 `baseRev` 乐观并发，`baseRev`
不符 → 409）、`lib/sync/sync_protocol.dart`（端口 47820、UDP 发现 47821、
魔串 `OVIMAP_SYNC/1`、照片分块 256KB、`/project/:id/photos/init|chunk|status|complete`、
`GET /ws` 进度推送）、`lib/sync/server/sync_server.dart`。

- **MVP 不要求 CadCraft 实现 SyncApi 客户端**（不读写 collection `payload`）。
- 桥接方案（提议的新端点，实现方在 `sync_server.dart` 增加，与现有端点同
  鉴权、同信封、同 `baseRev` 乐观并发；CadCraft 只需最小 HTTP 客户端）：

| 端点（提议） | 方法 | 作用 |
|---|---|---|
| `/project/:id/cadcraft` | `PUT` | ovimap 写出 GeoJSON 文本到服务端（body = GeoJSON 文本，`baseRev` 乐观并发，409 处理同 `PUT /project/:id`） |
| `/project/:id/cadcraft` | `GET` | CadCraft 拉取最新 GeoJSON（返回 `{rev, updatedAt, geojson}`） |

- 流程：ovimap“发送到 CadCraft”→ `PUT /project/:id/cadcraft`；CadCraft
  “从 ovimap 拉取”→ `GET /project/:id/cadcraft` → 按 §6 导入。
  照片走现有照片分块端点（ovimap 侧已有；CadCraft MVP 不必实现照片上传）。
- 局域网发现（可选）：CadCraft 监听 UDP 47821 的 `OVIMAP_SYNC/1` 信标，
  发现服务端地址；或用户手动填服务端地址（与 ovimap 配对流程一致）。
- 鉴权：与 ovimap 同步相同的 per-account Bearer token；CadCraft 侧由用户粘贴
  到设置里（类比 ovimap 配对），存入系统安全存储，**绝不**硬编码、绝不进文件。
- 若桥接未实现，文件交换（§7.1）即为完整 MVP；桥接缺席不得阻塞文件通道。

### 7.4 安全与隐私

- GeoJSON/DXF 是明文工程坐标文件：不含 token（§7.1），但含真实位置；只经
  可信渠道传输，不得发到公开网盘/群聊。
- 控制通道只监听 `127.0.0.1`（本地回环），不暴露到局域网；MCP 同理。
- 同步桥接走 HTTPS（经 Cloudflare Tunnel）或局域网；token 只走
  `Authorization` 头，不进 URL、不进文件、不进日志。

## 8. Phase 2：实时 socket 双向同步设计（设计，不做实现）

### 8.1 架构

```
ovimap（手机/桌面）                同步服务端（Windows，shelf）
   │  WebSocket（/cadcraft/ws，见 §8.2）   │
   ├───────────── hello / ops ─────────────┤
   │                                        │
CadCraft（桌面） ── WebSocket（同一端点）───┤  per-feature rev 日志 + 快照
```

- 单一服务端为序：服务端给每个操作分配单调递增 `seq`；客户端按 `seq` 顺序应用。
- 传输单位：**整个 Feature**（GeoJSON Feature + `ovimap_georef` 引用），不做
  顶点级 OT/CRDT（几何合并无可靠自动语义）。
- 服务端保留：最新快照（全量 Features）+ 最近 N 条操作日志（建议 N=1000，
  超出则只提供快照）；客户端断线重连时按 `lastSeenSeq` 续传，缺口超出日志
  则拉快照（见 §8.4）。

### 8.2 Wire 协议（JSON 消息，复用现有 `/ws` 思路，新端点）

- 端点（提议）：`GET /cadcraft/ws`（WebSocket 升级；与现有 `GET /ws` 进度推送
  分开，避免互相干扰）。鉴权：握手时 `Authorization: Bearer <token>`。
- 消息信封：`{seq?, type, opId, deviceId, updated_at, payload}`。

| type | 方向 | 含义 |
|---|---|---|
| `hello` | 客户端 → 服务端 | `{clientId, lastSeenSeq}`，服务端回 `welcome {serverSeq, snapshotNeeded}` |
| `upsert` | 客户端 → 服务端 | `{feature}`（完整 Feature，含 `ovimap_id`）；服务端分配 `seq` 后广播 |
| `delete` | 客户端 → 服务端 | `{ovimap_id}` |
| `op` | 服务端 → 客户端 | 广播已排序的操作（`upsert`/`delete` + `seq`） |
| `snapshot` | 服务端 → 客户端 | 全量 `{features, serverSeq}`（初次同步 / 缺口过大时） |
| `ack` | 客户端 → 服务端 | `{lastSeenSeq}`（已应用到的 seq，供服务端 GC 日志） |
| `ping` / `pong` | 双向 | 心跳，30 s 间隔（见 §8.4） |

### 8.3 冲突解决（要素级 LWW + 冲突留痕）

- 每要素版本向量：`(ovimap_id, updated_at, deviceId)`；服务端按 `updated_at`
  比较，新者胜；`updated_at` 相等时按 `deviceId` 字典序大者胜（确定性平局打破）。
- 失败方（被覆盖的版本）不丢：服务端将其存为
  `conflicts/<ovimap_id>@<updated_at>-<deviceId>.geojson`，并在广播中附带
  `conflict: {loserOpId, conflictFile}` 提示；用户手动取回。
- 删除与更新冲突：删除操作带 `baseRev`（该要素最后已知版本）；若服务端要素
  版本已前进 → 删除被拒绝并回 `conflict`，由用户裁决（删除永不静默赢）。
- 与 MVP 的关系：MVP 的“ovimap 赢 + 工程级粒度”（§6.4）是 Phase 2 的退化版；
  Phase 2 将粒度细化到要素级，并以 `updated_at`/`deviceId` 替代工程级比较。

### 8.4 断线重连与离线

- 客户端本地 outbox：未发送的操作持久化（SQLite/文件），每条带全局唯一 `opId`
  （UUID v4）；重连后按原顺序重发，服务端按 `opId` 去重（重发不产生双份）。
- 重连：指数退避（1s → 2s → 4s → … 上限 60s），每次重连发 `hello` 携
  `lastSeenSeq`；服务端对比日志：缺口在日志内 → 补发 `op`；缺口超出 → 发
  `snapshot`，客户端用快照重置本地状态（重置前本地未发送的 outbox 操作在快照
  应用后重放，重放冲突按 §8.3 处理）。
- 心跳：30 s `ping`，连续 3 次无 `pong` 视为断线；UI 显示“离线/同步中/已同步”
  三态，不得静默假装在线。
- 多端在线：同一账号多设备同时在线时，服务端广播给除发送方外的全部客户端；
  发送方以自己 `opId` 的回执确认，不重复应用自己的操作。

### 8.5 Phase 2 非目标

- 不做多人实时光标/选中同步；不做顶点级协同编辑；不做离线分支合并 UI；
  不把 Phase 2 的 socket 用于照片二进制（照片仍走分块上传）。

## 9. 瓦片：CadCraft 默认 OSM + 署名 + 缓存 + 失败不崩溃

### 9.1 默认瓦片（唯一默认）

- URL 模板：`https://tile.openstreetmap.org/{z}/{x}/{y}.png`（与 ovimap
  `osmStandard` 相同，`datum=0` WGS84，无偏移）。
- 级别：`z` ∈ [0, 19]；超过 19 时放大显示已有瓦片，**不得**向 OSM 请求 z>19。
- 并发：≤4；User-Agent 必须为应用真实标识（如 `CadCraft/<版本> (联系方式)`），
  遵守 OSM Tile Usage Policy（不批量下载、不抓取、不冒充浏览器 UA）。

### 9.2 署名（必须，三处同时）

- 状态栏常驻：`© OpenStreetMap 贡献者`。
- 图层属性/关于面板：完整署名 + 链接（`https://www.openstreetmap.org/copyright`）+
  数据许可说明（OSM 数据 © OpenStreetMap 贡献者，ODbL）。
- 导出/打印含底图时：图面角落带同样署名。
- 关闭/隐藏底图时署名可随之隐藏；只要底图可见，署名必须可见。

### 9.3 瓦片与矢量的坐标一致性

- 瓦片是 Web Mercator（EPSG:3857）渲染的栅格，仅作**底图显示**；矢量要素
  用 §3 球面 TM 投影。CadCraft 贴瓦片时：取瓦片四角 WGS84 → §3 球面 TM →
  贴图四边形。矢量与瓦片用**同一套**球面模型，因此两者对齐；两者相对
  WGS84 椭球真值的偏差（§3.5，50 km 约 196 m）是**一致**的，不会互相错位。
- 用户自填的瓦片源默认按 WGS84 对齐处理；若其源实际是 GCJ-02 坐标系，
  应在图源管理中将坐标类型设为 `Mercator(china)`（见 §9.6），CadCraft
  对该图源做**仅显示**的 GCJ-02 换算（瓦片角点 GCJ-02 → WGS84 再投影，
  视图范围 WGS84 → GCJ-02 再查瓦片）；存储（GeoJSON / DXF / 图纸）
  一律保持 WGS84，换算永不污染存储。若不设置，仍按 WGS84 对齐，会系统
  性错位约 300–600 m（§1.3）。

### 9.4 缓存与失败

- 磁盘缓存：键 `(sourceId, z, x, y)`，LRU，上限默认 500MB（可配置）；缓存
  有效期默认 7 天，过期后后台 revalidate，失败则继续用旧缓存。
- 失败：单张瓦片请求失败（网络/超时/HTTP 非 2xx）→ 显示空白占位，指数退避
  重试（上限 3 次），**永不**抛到 UI 线程、永不崩溃、永不无限重试循环。
- 离线：只读缓存，不弹错、不阻塞绘图；矢量要素始终可用（瓦片失败不影响
  矢量导入/导出/同步）。

### 9.5 用户自填瓦片 URL（授权责任）

- CadCraft 提供“图源管理”对话框（工具栏“在线地图”旁、“工具 ▸ OviMap ▸
  图源管理…”）：图源列表（名称 / 坐标类型 / 最高级别 / 署名）+ 新增 /
  编辑 / 删除 / 设为当前 + 确定 / 取消；不只留命令。确定后图源库随
  应用配置保存、启动加载，当前图源经 `tilemap` 命令应用到图纸。
- CadCraft 提供“自定义瓦片源”设置：URL 模板（含 `{z}/{x}/{y}` 或奥维
  式 `{$z}/{$x}/{$y}`，`{$s}` / `{$Galileo}` 为子域名轮换 `(x+y)%4`）
  + 署名字段（**必填**，为空时拒绝启用）+ 最大级别 + 坐标类型。
- **授权责任在用户**：用户自填的 URL 须是用户有权使用的服务；CadCraft
  不审核、不背书。自定义源的署名由用户填写，CadCraft 原样显示。
- **红线（硬性，测试 T8 断言）：**
  - CadCraft 代码、默认配置、测试、fixture 里**绝不**硬编码 sydt
    （`sydt.hainasi.eu.org`）、高德（`*.is.autonavi.com` / `*.amap.com`）
    或任何其他专有图源 URL，也**绝不**拉取其瓦片。
  - 不得默认使用 ovimap 的 `osm-proxy`（`hzyt.hainasi.eu.org`，用户自建反代，
    是用户个人基础设施，不是公共默认）。
  - 测试不得请求真实专有瓦片：用本地 stub 瓦片服务器或缓存 fixture。

### 9.6 自定义图源：奥维 XML 导入与 GCJ-02 显示换算

- 导入：图源管理对话框可导入奥维格式 `customMapSource` XML，解析
  `mapID` / `name` / `version` / `maxZoom` / `coordType` / `tileType` /
  `tileFormat` / `tileSize` / `protocol` / `host` / `port` / `url`。
  `mapID` / `name` / `url` 必填；`url` 为绝对地址时直接使用，为相对
  路径时由 `protocol` + `host`（+ 非默认 `port`）补齐。XML 中没有署名
  字段，导入时须由用户另填署名，无署名拒绝导入；缺必填字段、非法
  `host`、非法模板均拒绝并中文提示原因。
- `coordType`：`Mercator` 按 WGS84 球面墨卡托（与矢量同框，无换算）；
  `Mercator(china)` 按 GCJ-02 **仅作显示换算**（§9.3），不污染存储。
  默认 OSM 图源为 WGS84，不可设为 `Mercator(china)`。
- 切换图源后瓦片缓存按键隔离：缓存目录由 URL 模板哈希决定，不同图源
  的瓦片永不互相覆盖（同一模板的不同图源共享字节缓存是正确的：瓦片
  字节与坐标类型无关，只有贴图位置不同）。
- 内置图源只有 OSM 与 CARTO（均为 OSM 数据），不可删除、不可改 URL /
  署名；图源库随应用配置保存，启动加载。

## 10. 测试计划

### 10.1 样例工程（T1–T7 共用，固定数据，见 §2.4 形状）

| 内容 | 数量 | 构成 |
|---|---|---|
| 标记 | 12 | 水泥杆 4、电杆 2、人孔 2、分纤盒 2、分光器箱 1、交接箱 1（其中 2 个带照片，`photos[]` 各 1 个文件名；1 个带 `segCable`/`slackM`；1 个带 `holes`/`usedHoles`） |
| 轨迹 | 3 | 5 顶点杆路链（含 `distanceM`/`segKind`/`segCable` 顶点）、3 顶点链、1 条闭合区域环（`closedArea`，坐标首尾重复） |
| 标签 | 4 | 纯文字点 4 个（其中 1 个含换行 → MTEXT） |
| 照片钉 | 1 | `kind=photo`，`photos=["IMG_009.jpg"]` |
| 照片文件 | 3 个文件名 | 仅文件名进文件；其中 1 个故意缺失（测 §5.5 缺失提示） |

### 10.2 测试用例

| 编号 | 名称 | 步骤 | 断言 |
|---|---|---|---|
| T1 | Schema 校验 | 用 JSON Schema（由 §2.3 生成）校验样例文件；另造 3 个坏文件：越界经纬度、轨迹 `vertices` 长度 ≠ 坐标数、`timestamps` 数组长度 ≠ 坐标数 | 好文件通过；3 个坏文件均被拒绝，且报错指明原因 |
| T2 | GeoJSON 往返（ovimap） | 样例 → ovimap 导入 → ovimap 导出 → 对比 | 要素数、全部 `ovimap_id`、坐标 ≤1e-9°、业务字段（`segCable`/`slackM`/`holes`/`distanceM`）一致；重复导入同一文件 → 0 改动（幂等） |
| T3 | DXF 往返（CadCraft 保存后） | 样例 → CadCraft 导入 → CadCraft 保存 DXF → 再导入 ovimap | 要素数、`ovimap_id`、几何 ≤1mm 一致；轨迹顶点 ID 保留（VERTICES_* XRECORD 存活）；DXF 不覆盖 ovimap 业务字段 |
| T4 | DXF 降级（XRECORD 被剥离） | 取 T3 的 DXF，删掉 VERTICES_* XRECORD 后导入 | 标记 id 保留（xdata 存活）；轨迹顶点获新 ID；导入报告告警并列出受影响轨迹 |
| T5 | 删除语义 | 导入 `delete_mode="none"` + `ovimap_deletes=[某 id]` 的文件；再导入 `delete_mode="sync"` 的同一文件 | 前者：id 仍在 + 报告说明 deletes 被忽略；后者：id 被删除 |
| T6 | 冲突（ovimap 赢） | ovimap 侧改某要素后，导入 `exported_at` 更旧但内容不同的文件 | ovimap 版本保留；冲突 id 在报告中；CadCraft 改动未应用 |
| T7 | 控制通道 | 经 `engine.execute {command:"ovimap.import"}` 导入样例，再 `drawing.inspect` 核对 | 导入报告计数 = 样例计数；`inspect` 实体数匹配（12 标记×2 实体 + 3 轨迹 + 4 标签 + 照片提示）；未注册命令时报“命令不存在” |
| T8 | 瓦片红线与容错 | grep CadCraft 源码/默认配置/测试：`sydt` / `autonavi` / `amap` / `hainasi` 零命中（除本文档）；启动本地 stub 瓦片服务器 + 断网两种场景 | 默认 URL 仅为 OSM；署名三处可见；断网时缓存可用、矢量不受影响；单张瓦片失败不崩溃 |
| T9 | GCJ 换算方向 | 用 §1.3 向量：WGS84 (32.1301, 114.0814) → GCJ-02 应为 (32.1281536, 114.0872867)；反向还原 | 正向/反向均在 1e-6° 内；含 GCJ 坐标直写的 GeoJSON（人为构造）被人工复核流程拦截——代码层面：写出前必须经 `toWgs84`（以写出端单测覆盖） |
| T10 | 投影数值 | 实现 §3.2/§3.3；按 §3.5 采样方法（原点 32.1301/114.0814，半径 {1,5,10,50} km，方位 1° 步进）对比球面 vs 椭球 TM | 往返误差 <1mm；各半径最大误差与 §3.5 表一致（±2 m 容差）；50 km 最大值落在 [190, 200] m 区间 |

### 10.3 验收标准

- T1–T10 全部通过；两份文档字节一致（`diff` 无输出）。
- 样例工程对数表（导入前后）：标记 12 / 轨迹 3 / 标签 4 / 照片钉 1，
  导出再导入后计数不变，`ovimap_id` 集合不变。
- 导入报告格式统一：`{added, updated, skipped, conflicts[], missingPhotos[], warnings[]}`，
  两侧（ovimap / CadCraft）同形，便于对照。

## 附录 A：实现接口约定清单（实现方按此对接）

| 编号 | 约定 | 落点 |
|---|---|---|
| C1 | 交换文件：`<工程名>.ovimap.geojson`（UTF-8，LF），顶层 `FeatureCollection` + 外来成员 `ovimap_georef` / `ovimap_meta` / `ovimap_deletes` | §2.1–2.2 |
| C2 | Schema 版本 `"1.0"`；未知大版本拒绝，未知字段透传 | §2.1、§2.6 |
| C3 | 要素身份 = `ovimap_id`；轨迹 `ovimap_id` = `lineGroupId`；同文件内轨迹 id 与标记 id 重合 → 拒文件 | §2.3、§2.5.2 |
| C4 | `kind` ∈ {`marker`,`track`,`label`,`photo`}；推导规则见 §2.5；`kind=photo` 仅 CadCraft 原生钉 | §2.5 |
| C5 | 轨迹：`vertices[]` 与坐标对齐；`timestamps` 为 `null` 或等长数组；`timestamps` 绝不编造 | §2.5.2 |
| C6 | `photos[]` 仅 basename；非法条目丢弃并告警；照片文件另行流转 | §2.5.3、§5.5 |
| C7 | `created_at`/`updated_at` 来源：工程级时间或保存值，绝不逐要素编造；不是同步主键 | §2.7 |
| C8 | 投影：`OVIMAP_SPHERICAL_TM`，R=6378137，k₀=1，false=0，原点=工程中心；正/逆算公式见 §3.2–3.3；遇 `OVIMAP_ELLIPSOIDAL_TM` 拒绝 | §3 |
| C9 | 50 km 误差表（§3.5）为测试 oracle：球面 vs 椭球最大 195.85 m（正南），Web Mercator 9417.80 m；往返 <1mm | §3.5、T10 |
| C10 | DXF：R2000+，`$INSUNITS=6`，APPID `OVIMAP`，OBJECTS `OVIMAP_GEOREF` 字典 → `GEOREF` XRECORD（组码顺序固定）+ `VERTICES_<lineGroupId>` XRECORD（1070 计数 + 1000×N） | §4.2–4.3 |
| C11 | 实体 xdata：`1001 OVIMAP` + `1000 ovimap_id/name/kind/type`（顺序固定，1000 单条 ≤255 字符） | §4.4 |
| C12 | 图层：`OVIMAP_MARKER` / `OVIMAP_TRACK` / `OVIMAP_LABEL` / `OVIMAP_PHOTO`；不存在则创建，永不删用户图层 | §4.5 |
| C13 | 映射：标记→POINT+TEXT，轨迹→LWPOLYLINE（含闭合标志），标签→TEXT/MTEXT，照片→POINT+TEXT 提示；POINT/LWPOLYLINE 几何权威，TEXT 位置装饰 | §5 |
| C14 | 同步：按 `ovimap_id` upsert；删除仅 `ovimap_deletes` + `delete_mode="sync"`；默认不删；冲突 ovimap 赢 + 报告；重复导入 0 改动 | §6 |
| C15 | 控制通道：复用 `127.0.0.1:PORT` JSON Lines + MCP；新增命令 `ovimap.import` / `ovimap.export` / `ovimap.inspect`（以 `engine.commands` 为准）；复用 `app.open` / `drawing.inspect` / `document.bytes` | §7.2 |
| C16 | Sync 桥接（可选 Phase 1.5）：新端点 `PUT/GET /project/:id/cadcraft`，同鉴权/信封/`baseRev`；现有 SyncApi 端点语义不变；CadCraft 不读写 collection `payload` | §7.3 |
| C17 | 瓦片：默认仅 `https://tile.openstreetmap.org/{z}/{x}/{y}.png`（z≤19，并发≤4，真实 UA）；三处署名；500MB LRU 缓存；失败不崩溃；自定义源须填署名；绝不硬编码 sydt/高德/反代 URL | §9 |
| C18 | GCJ-02 测试向量：WGS84 (32.1301, 114.0814) ↔ GCJ-02 (32.1281536, 114.0872867)，偏移约 595.7 m；GeoJSON 一律 WGS84 | §1.3、T9 |
| C19 | 导入报告同形：`{added, updated, skipped, conflicts[], missingPhotos[], warnings[]}` | §10.3 |
| C20 | 两份文档字节一致；改一处必须同步另一处 | 本文头 |

## 附录 B：未决与风险（实现前确认）

- **R1** CadCraft 保存 DXF 后 OVIMAP XRECORD 是否存活：需 T3 实测验证；若不存活，
  DXF 路径按 §4.6 降级（顶点重编 ID），并在 UI 提示“请用 GeoJSON 往返”。
- **R2** `lineGroupId` 字符集与 DXF 字典键兼容性：若某 `lineGroupId` 含 DXF
  字典键不接受的字符，导出必须报错（§4.3），不得静默改名。
- **R3** CadCraft 是否有 HTTP 客户端能力（§7.3 桥接依赖）：若无，桥接推迟，
  文件通道仍为完整 MVP。
- **R4** TEXT 字高 2.5 米在全图缩放下不可见是预期行为（§5.2），需在 CadCraft
  导入完成提示中说明，避免用户误报“文字丢失”。
- **R5** MVP 冲突粒度为工程级（§6.4），要素级 LWW 留给 Phase 2（§8.3）；
  MVP 阶段用户需知悉“双方同时改同一工程时 CadCraft 侧改动会被丢弃并报告”。
- **R6** ovimap 现有 DXF 出图（等距圆柱近似）与联动 DXF（球面 TM）是两条路径，
  菜单/函数必须分开，不得互相覆盖（§3.6）。
