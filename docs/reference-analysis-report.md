# Reference 项目全面分析报告

> 分析日期：2026-06-13  
> 覆盖项目：`references/` 下全部 9 个开源项目  
> 目的：识别可借鉴的技术方案，提升 disk_organizer 的分类准确性、扫描性能和用户体验

---

## 总览

| 项目 | 语言 | 用途 | 对我们的价值 |
|------|------|------|-------------|
| **Dism++** | C++/XML | Windows 优化工具，含数百条清理规则 | ⭐⭐⭐⭐⭐ 规则库 |
| **BleachBit** | Python/XML | 跨平台清理工具，~120 条规则 | ⭐⭐⭐⭐ 软件路径 |
| **dua-cli** | Rust | 终端磁盘分析工具 | ⭐⭐⭐⭐ 扫描架构 |
| **dust** | Rust | 终端磁盘分析工具 | ⭐⭐⭐ 并发扫描 |
| **diskonaut** | Rust | 交互式 TUI 磁盘分析 | ⭐⭐ TUI 交互 |
| **pdu** | Rust | 并行磁盘分析 | ⭐⭐⭐ 并行策略 |
| **colinfinck-ntfs** | Rust | 底层 NTFS 解析库 | ⭐⭐⭐⭐ MFT 直读 |
| **omerbenamram-mft** | Rust | MFT 解析 + NTFS 工具链 | ⭐⭐⭐⭐ MFT 直读 |
| **WinDirStat** | C++ | 经典 Windows 磁盘分析 | ⭐⭐⭐ 分类+树图 |
| **NTFS-File-Search** | C++ | MFT 直读快速搜索 | ⭐⭐ MFT 性能验证 |

---

## 一、Dism++ Data.xml — 规则库分析

### 1.1 完整的分类体系（Group 值）

通过 XML 注释和组织结构，Dism++ 定义了以下**6 大类**：

| 分类 (Group) | 中文含义 | 我们的对应分类 |
|-------------|---------|-------------|
| `#过期文件` | 被取代/过期的系统组件和软件版本 | 可清理/系统过时组件 |
| `#系统相关` | Windows 系统级路径 | 系统文件 |
| `#缓存文件` | 各种软件的安装源缓存和运行时缓存 | 缓存文件 |
| `#备份文件` | 系统或软件创建的备份 | 备份文件 |
| `#临时文件` | 临时文件、日志、驱动解压 | 临时文件 |
| `#应用程序` | 应用程序安装源和运行时数据 | 软件安装源 |

### 1.2 🔥 与我们的误分类直接相关的关键规则

#### 驱动解压目录（这是根因！）
```xml
<Item Name="#常见的驱动临时解压目录" Level="3">
  <Description>#Intel、AMD以及Nvidia驱动在安装时留下的解压目录。</Description>
  <Group>#临时文件</Group>
  <Activate>
    <General RootPath="%SystemDrive%" Flags="Directory">
      <Query>AMD</Query>
      <Query>Intel</Query>
      <Query>NVIDIA</Query>
      <Query>Prog</Query>
    </General>
  </Activate>
</Item>
```
**发现**：`C:\AMD`、`C:\NVIDIA`、`C:\Intel`、`C:\Prog` 被 Dism++ 明确标记为**驱动安装时留下的解压目录**（临时文件）。

这直接解释了为什么 `C:\eSupport\eDriver\...\AMD\AMD_GraphicDriverOnly_ROG\...` 应该被识别为驱动相关，而非「游戏数据」。

#### NVIDIA 安装源缓存
```xml
<Item Name="#英伟达驱动安装源缓存" Level="0">
  <Group>#缓存文件</Group>
  <Activate>
    <General RootPath="%ProgramFiles%\NVIDIA Corporation\Installer2"/>
    <General RootPath="%SystemDrive%\users\*\appdata\nvidia\nvbackend\packages"/>
  </Activate>
</Item>

<Item Name="#英伟达驱动安装包" Level="1">
  <Group>#临时文件</Group>
  <Activate>
    <General RootPath="%PROGRAMDATA%\NVIDIA Corporation\Downloader"/>
  </Activate>
</Item>
```

#### AMD 相关
```xml
<Item Name="#常见的驱动临时解压目录">
  <Query>AMD</Query>  <!-- C:\AMD -->
</Item>
```

#### Intel 驱动
```xml
<Item Name="#Intel驱动安装源缓存" Level="0">
  <Group>#缓存文件</Group>
  <General RootPath="%ProgramData%\Intel\Package Cache"/>
</Item>
```

#### 瑞昱声卡
```xml
<Item Name="#瑞昱声卡驱动安装源缓存" Level="0">
  <Group>#缓存文件</Group>
  <General RootPath="%ProgramFiles%\Realtek\Audio" Flags="Directory">
    <Query>*</Query>
    <Excluded>HDA</Excluded>
    <Excluded>ASIO</Excluded>
  </General>
</Item>
```

### 1.3 国产软件覆盖

Dism++ 覆盖了大量国产软件路径，这对中文 Windows 用户非常重要：

| 软件 | 规则 |
|------|------|
| QQ | `%AppData%\Tencent\Logs`, `TXSSO`, `WinTemp` |
| YY | `%AppData%\duowan\yy\log`, `Cache` |
| 百度网盘 | `%APPDATA%\BaiduYunKernel\Data`, `BaiduYunGuanjia\logs` |
| 360浏览器 | `%APPDATA%\360se6\Application\*.*.*.*` (老版本) |
| 酷狗音乐 | `HKCU\Software\kugou`, 老版本备份清理 |
| 阿里旺旺 | 老版本备份清理 |
| 阿里亲淘 | 老版本备份清理 |
| WPS | 老版本备份清理 |
| 2345输入法 | 老版本备份清理 |
| PPLive | 老版本备份清理 |
| 飞信 | `%APPDATA%\FetionV5\` |

### 1.4 系统路径覆盖

Dism++ 还覆盖了 Windows 系统级路径：

- `%SystemDrive%\Windows.old*` — 旧 Windows
- `%SystemRoot%\WinSxS\ManifestCache` — WinSxS 缓存
- `%SystemRoot%\SoftwareDistribution\*` — Windows Update 缓存
- `%SystemRoot%\Prefetch` — 预读取文件
- `%SystemRoot%\Installer\$PatchCache$` — Installer 缓存
- `%ProgramData%\Microsoft\Windows Defender\Scans\History` — Defender 历史
- `%SystemDrive%\$Windows.~BT` / `$Windows.~WS` — 系统临时安装
- `%System%\winevt\Logs` — Windows 事件日志
- `%SystemRoot%\MEMORY.DMP`, `Minidump` — 崩溃转储
- `%SystemRoot%\assembly\NativeImages_*` — .NET 程序集缓存

### 1.5 Dism++ 规则引擎要点

- **Applicable 前置条件**：规则执行前先检查注册表/文件是否存在（如 NVIDIA 规则需 `FileExist FilePath="%ProgramFiles%\NVIDIA Corporation"`），避免扫描不存在的路径
- **Smart 函数**：支持 `?GetRegSz(key, value)` 动态从注册表读取路径，`?GetFileVersion()` 版本比较
- **Level 分级**：Level 0 = 专家模式（高风险），Level 1 = 默认勾选，Level 2 = 普通，Level 3 = 低级
- **Excluded 排除**：支持排除特定路径/版本
- **OSVersion**：根据 Windows 版本激活不同规则

---

## 二、BleachBit — 软件路径覆盖

BleachBit 提供 **104 个 XML 文件**，覆盖了大量常用软件：

### Windows 特有规则
| 软件 | 路径模式 | 分类 |
|------|---------|------|
| Google Chrome | `%LocalAppData%\Google\Chrome\User Data\*\Cache` | 浏览器缓存 |
| Microsoft Edge | `%LocalAppData%\Microsoft\Edge\User Data\*\Cache` | 浏览器缓存 |
| Firefox | `%AppData%\Mozilla\Firefox\Profiles\*\cache2` | 浏览器缓存 |
| Brave/Chromium/Vivaldi/Opera | 各种 Chromium 系路径 | 浏览器缓存 |
| Discord | `%AppData%\discord\Cache`, `Code Cache` | 聊天缓存 |
| Slack | `%AppData%\slack\Cache`, `Code Cache` | 聊天缓存 |
| Skype | `%AppData%\Skype\*\media_messaging\storage_db` | 聊天缓存 |
| Zoom | `%AppData%\Zoom\data\*\the.web.meeting.recordings` | 录制文件 |
| VS Code | `%AppData%\Code\Cache`, `CachedData`, `User\workspaceStorage` | 开发工具缓存 |
| Microsoft Office | `%LocalAppData%\Microsoft\Office\*\OfficeFileCache` | Office 缓存 |
| Java | `%AppData%\Sun\Java\Deployment\cache` | Java 缓存 |
| TeamViewer | `%AppData%\TeamViewer\*_Logfiles` | 远程工具日志 |
| LibreOffice | `%AppData%\libreoffice\*\cache` | Office 缓存 |
| WinRAR | `%AppData%\WinRAR\version.txt` (临时)，历史列表 | 压缩工具 |
| VLC | `%AppData%\vlc\art` (封面缓存) | 媒体播放器 |
| Windows Explorer | `%AppData%\Microsoft\Windows\Recent` | 最近文件 |
| Windows Defender | `%ProgramData%\Microsoft\Windows Defender\Scans` | 安全软件 |
| Windows Media Player | `%LocalAppData%\Microsoft\Media Player` | 媒体缓存 |
| Thumbnails | `%LocalAppData%\Microsoft\Windows\Explorer\thumbcache_*.db` | 缩略图 |

### BleachBit 规则格式
```xml
<cleaner id="discord">
  <label>Discord</label>
  <var name="base">
    <value os="windows">%AppData%\discord</value>
    <value os="linux">~/.config/discord</value>
  </var>
  <option id="cache">
    <label>Cache</label>
    <action command="delete" search="walk.all" path="$$base$$/Cache"/>
    <action command="delete" search="glob" path="$$base$$/Code Cache"/>
  </option>
</cleaner>
```

**BleachBit 的优势**：
- 多 OS 支持（Windows/Linux/macOS），变量自动切换
- `walk.all` 遍历适用于递归清理
- `glob` 模式用于精确匹配

---

## 三、Rust 磁盘扫描工具 — 架构借鉴

### 3.1 dua-cli（⭐⭐⭐⭐ 最有价值）

**核心架构**：
```
jwalk (并行文件遍历)
  → crossbeam::channel (解耦 IO 和 UI)
    → petgraph::StableGraph<EntryData> (有向图存储目录树)
      → 深度滚动聚合 (增量 bottom-up 大小累加)
```

**关键技术**：

| 技术 | 实现 | 我们可借鉴 |
|------|------|-----------|
| **并行遍历** | `jwalk::Parallelism::RayonNewPool`，自定义线程数 | 当前项目用 walkdir（单线程），可升级 |
| **目录树存储** | `petgraph::StableGraph`，节点 index 永不失效 | 当前用简单的 Vec/HashMap，改为图结构可支持增量更新 |
| **大小聚合** | 深度跟踪栈 + bottom-up 滚动 | 当前 top-down 累加，改为 bottom-up 更准确 |
| **硬链去重** | `InodeFilter`: `HashMap<(dev,ino), remaining_links>` | ⭐ 关键：当前缺少，会导致重复计算硬链文件 |
| **跨盘限制** | `crossdev::is_same_device()` 过滤 | ⭐ 避免跨磁盘链接导致的错误大小 |
| **异步架构** | 后台线程遍历 → crossbeam channel → UI 线程 | 当前同步阻塞，可改为 channel 模型 |

**InodeFilter 算法**（硬链去重）：
```rust
// 平台无关的 inode 表示
HashSet<(u64, u64)> // (device_id, file_index on Windows)

// 首次遇到：记录并计入
// 再次遇到：递减 nlink 计数，跳过
// 计数归零：从 set 移除
```

### 3.2 dust

- **更简洁的实现**：~3000 行 vs dua-cli ~8000 行
- **并行策略**：使用 `rayon` 的 `par_iter` 在目录级别并行
- **显示**：类似 `du` 但带彩色柱状图，反转树（最深路径在上）
- **平台适配**：使用 `get_metadata()` 而非 `symlink_metadata()`

### 3.3 diskonaut

- **TUI 框架**：`tui-rs` (现 `ratatui`)
- **交互式导航**：方向键浏览目录树，Enter 进入子目录，Backspace 返回
- **实时渲染**：扫描过程实时显示中间结果
- **可视化**：矩形块面积 = 文件/目录大小

### 3.4 parallel-disk-usage (pdu)

- **纯 Rayon 并行**：在目录入口处 `par_bridge()` 派生子任务
- **文件大小**：使用 `std::fs::symlink_metadata().len()`（不跟随符号链接）
- **进度条**：`indicatif` 实时显示扫描进度
- **输出格式**：类 `du` 的树形文本输出

---

## 四、NTFS 底层库 — 性能突破可能

### 4.1 colinfinck-ntfs（`ntfs` crate v0.4.0）

**核心能力**：
- ✅ **`no_std` 兼容**：可用于内核态到用户态
- ✅ **直接读取 MFT**：`Ntfs::new(&mut fs)` 打开分区/镜像
- ✅ **枚举所有文件**：`root_dir.directory_index()` → `index.entries()` 遍历
- ✅ **O(1) 目录迭代**：利用 NTFS B-tree 索引顺序遍历
- ✅ **大小写不敏感搜索**：遵循 `$Upcase` 表
- ✅ **读取所有 ADS**（Alternate Data Streams）
- ✅ **平台无关**：Windows/Linux 均可读取 NTFS 镜像

**不支持的**：写入、压缩、加密、重解析点

**API 示例**：
```rust
let mut ntfs = Ntfs::new(&mut fs).unwrap();
let root_dir = ntfs.root_directory(&mut fs).unwrap();
let index = root_dir.directory_index(&mut fs).unwrap();
let mut iter = index.entries();
while let Some(entry) = iter.next(&mut fs) {
    let file_name = entry.key().unwrap().name();
    println!("{}", file_name);
}
```

### 4.2 omerbenamram-mft（`mft` crate v0.7.0）

**特点**：
- **100% safe Rust**，跨平台
- **工具链完整**：含 `mft_dump` 二进制 + `ntfs` workspace crate + `ntfs-explorer-gui`
- **输出格式**：JSON、JSONL、CSV
- **提取常驻数据流**
- **性能剖析**：内置 benchmark，用 samply 做 CPU profiling
- **注意**：这个项目的 `ntfs` crate 与 colinfinck-ntfs 的 `ntfs` crate 命名冲突

### 4.3 MFT 直读 vs 传统遍历的性能对比

基于 NTFS-File-Search 和 omerbenamram-mft 的数据：
- **传统 `std::fs::read_dir`**：每个目录需要一次系统调用，深层嵌套极慢
- **MFT 直读**：读取 `$MFT` 文件（几十MB），所有文件元数据直接可用
- **预估加速**：对于百万级文件的磁盘，MFT 方式可能快 **10-50x**

**坑**：
- 需要管理员权限才能读取 `\\.\C:` 原始分区
- 需要解析 `$MFT` 结构并重建目录树
- NTFS 版本兼容性（3.0/3.1）

---

## 五、WinDirStat — 经典分类方案

（基于文件扩展名分类 + 树图可视化）

### 5.1 关键理念
- **扩展名映射**：`.exe` → 应用程序，`.dll` → 系统库，`.mp4` → 视频
- **可视化**：Cushion Treemap（KDirStat 发明）
- **目录矩形面积 = 占用空间**，子矩形 = 子目录/文件

### 5.2 对我们分类的启示
- 文件扩展名分类是**目录路径分类的补充**：当目录语义不明确时，可以用目录内文件类型来推断
- 例如：一个目录内 80% 是 `.lib`/`.obj`/`.pdb` → 很可能是编译产物

---

## 六、综合建议：按优先级排序

### P0 — 立即可做（低成本高收益）

| # | 改进项 | 来源 | 工作量 |
|---|--------|------|--------|
| 1 | **扩充 catalog.rs 驱动规则** | Dism++ | 1h |
|   | 添加 `C:\AMD`, `C:\NVIDIA`, `C:\Intel`, `C:\Prog` → 驱动程序/临时文件 | | |
|   | 添加 `C:\eSupport\eDriver` → 驱动安装程序 | | |
| 2 | **扩充 catalog.rs 软件路径** | Dism++ + BleachBit | 2h |
|   | QQ, YY, 百度网盘, 360浏览器, 酷狗, 阿里旺旺, WPS 等 | | |
|   | Discord, Slack, Zoom, VS Code, Chrome, Edge 等 | | |
| 3 | **扩充 catalog.rs 系统路径** | Dism++ | 1h |
|   | WinSxS, SoftwareDistribution, Prefetch, Installer, Package Cache 等 | | |

### P1 — 短期改进（中等成本）

| # | 改进项 | 来源 | 工作量 |
|---|--------|------|--------|
| 4 | **丰富 LLM prompt few-shot** | 分析结果 | 30min |
|   | 为「驱动程序」「专业软件」「显卡组件」各加 CAUTION 示例 | | |
| 5 | **添加硬链去重（InodeFilter）** | dua-cli | 2h |
|   | 避免重复计算硬链文件大小 | | |
| 6 | **添加跨文件系统过滤** | dua-cli | 30min |
|   | 避免跨盘软链接导致的错误统计 | | |
| 7 | **扩展名级别分类补充** | WinDirStat | 2h |
|   | 当目录语义不明确时，用内容文件类型推断 | | |

### P2 — 中期改进（较大改动）

| # | 改进项 | 来源 | 工作量 |
|---|--------|------|--------|
| 8 | **并行扫描（jwalk 替代 walkdir）** | dua-cli | 4h |
|   | 多线程遍历加速扫描 | | |
| 9 | **MFT 直读快速通道** | colinfinck-ntfs | 8h |
|   | 管理员模式下绕过 OS 遍历，MFT 直读 | | |
| 10 | **增量扫描** | dua-cli 架构 | 6h |
|    | 利用 StableGraph 图结构实现只重扫变更目录 | | |

### P3 — 长期愿景

| # | 改进项 | 来源 | 工作量 |
|---|--------|------|--------|
| 11 | **交互式 TUI** | diskonaut | 20h+ |
| 12 | **树图可视化** | WinDirStat | 20h+ |
| 13 | **NTFS Change Journal 增量更新** | USN Journal | 12h |

---

## 七、具体可提取的路径模式

### 从 Dism++ Data.xml 提取的路径 → 分类映射

```
# 驱动相关
C:\AMD                                    → 驱动解压/临时文件
C:\NVIDIA                                 → 驱动解压/临时文件
C:\Intel                                  → 驱动解压/临时文件
C:\eSupport\eDriver                       → 驱动安装程序
%ProgramFiles%\NVIDIA Corporation\Installer2  → NVIDIA 缓存
%ProgramData%\NVIDIA Corporation\Downloader   → NVIDIA 临时安装包
%ProgramData%\Intel\Package Cache             → Intel 缓存
%ProgramFiles%\Realtek\Audio\[除HDA/ASIO外]   → 瑞昱驱动缓存

# 国产软件
%AppData%\Tencent\Logs                    → 腾讯日志/临时
%AppData%\duowan\yy\log                   → YY 日志/临时
%AppData%\BaiduYunKernel\Data             → 百度网盘缓存
%AppData%\FetionV5                        → 飞信日志/临时

# 系统路径
%SystemRoot%\WinSxS\ManifestCache         → 系统缓存
%SystemRoot%\SoftwareDistribution         → Windows Update 缓存
%SystemRoot%\Prefetch                     → 预读取缓存
%SystemRoot%\Installer\$PatchCache$       → Installer 缓存
%ProgramData%\Package Cache               → WIX 安装源
%ProgramData%\Microsoft VisualStudio\Packages → VS 安装源

# 常用软件（从 BleachBit）
%AppData%\discord\Cache                   → Discord 缓存
%AppData%\slack\Cache                     → Slack 缓存
%AppData%\Code\Cache                      → VS Code 缓存
%AppData%\Zoom\data                       → Zoom 录制/缓存
```

---

## 八、关键结论

1. **Dism++ Data.xml 是分类规则的金矿**：6 大类、数百条路径规则，且全部有中文描述可直接用作标签
2. **BleachBit 补充了国际软件覆盖**：104 个应用，跨平台路径定义规范
3. **dua-cli 的架构最值得学习**：jwalk 并行遍历 + petgraph 图结构 + InodeFilter 硬链去重 + crossbeam channel
4. **MFT 直读是性能突破的关键**：colinfinck-ntfs 和 omerbenamram-mft 都可用，预估 10-50x 加速
5. **WinDirStat 的扩展名分类可作为路径分类的补充**：当目录语义不明确时用内容推断
6. **当前最紧迫的问题是 catalog.rs 规则太少**：优先从 Dism++ 和 BleachBit 提取路径模式
