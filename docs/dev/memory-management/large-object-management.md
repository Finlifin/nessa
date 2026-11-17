# 大对象管理 (Large Object Management)

## 三层分配与双轨大对象角色 (`PayloadRole`)

Nessa 在保持常规对象 `16 B` 紧凑 `ObjectHeader`（`[TypeIndex: u32][gc_meta: u32][hash_or_fwd: u64]`）不变的前提下，结合线程本地小对象分配器（Unix 上为 MMTk Immix per-worker bump allocator + Large Object Space，Windows 上为精确 STW Mark-Sweep + Block/LOS 分配器）与堆外原子引用计数（RC），按“是否包含托管 `TaggedValue` 引用”采用双轨大对象策略：

| 角色 (`PayloadRole`) | 适用对象与阈值 | 堆内头与布局 | 单对象容量上限 | 回收与扫描机制 |
| :--- | :--- | :--- | :--- | :--- |
| `Ordinary` (`0`) | 常规含引用对象（`≤ 65,535` words）及小字符串（`≤ 8,168 B`） | `16B ObjectHeader` + `N × 8B` | `65,535` words（约 `512 KiB`） | `≤ 8 KiB` 走 Immix bump/overflow；`> 8 KiB` 走 MMTk LOS；按类型扫描引用槽位 |
| `ByteBuffer` (`2`) | 中小 `Bytes` 内部缓冲（`≤ 8,168 B`） | `16B ObjectHeader`（25-bit 字数）+ `[share_count: u64][bytes...]` | `MAX_BYTE_BUFFER_BYTES`（约 `256 MiB`，当前用于 `≤ 8 KiB` 内联缓冲） | Immix 快速分配，No-Scan |
| **`LargeTagged` (`3`)** | 超出 `u16::MAX` 槽位的大容器/大对象（`List` / `Map` / `Tuple` / `Struct` / `Enum`） | **`[payload_words: u64 (8B)]` + `16B ObjectHeader` + `N × 8B`**（`alloc_start = payload_ptr - 24`） | **`MAX_LARGE_TAGGED_WORDS` = `67,108,864` words（`512 MiB` 槽位）** | 直接进入 MMTk Treadmill LOS（页对齐、永不移动），完整保留 `gc_meta[22..=27]` 6-bit 并发锁分片 `stripe_index`，GC 扫描并转发内部所有 Immix 子引用 |
| **`ExternalRc` (`4`)** | 大 `String` 与大 `Bytes` 内部缓冲（`> 8,168 B`） | **Immix 40B 薄壳**：`16B ObjectHeader` + `[raw_ptr: u64, offset: u64, byte_len: u64]`；真实字节位于堆外 `ExternalRcHandle` | **`MAX_EXTERNAL_RC_BYTES` = `1 GiB`** | 堆内薄壳 No-Scan、完全兼容移动式 Immix 与多发延续 `k.clone()`；堆外缓冲由原子 `refs` 管理，支持大字符串 $O(1)$ 零拷贝切片共享，STW `WeakObject` 清扫与外部内存压力记账自动归还 OS 内存 |

## 关键设计不变量

### 1. `PayloadRole::LargeTagged`（前缀 `u64` 字数 + MMTk LOS）
- **零槽位偏移差异**：`u64` 字数存放在 `ObjectHeader` 之前的 8 字节（`payload_ptr - 24`），`ObjectHeader` 仍紧贴 `payload_ptr - 16`，`payload_ptr` 依然直接指向第 `0` 个 `TaggedValue` 槽位。解释器、内建容器和 `NessaScanning` 对 `Ordinary` 与 `LargeTagged` 共享完全一致的零偏移寻址。
- **并发条带锁保留**：无需借用 `gc_meta[22..=27]` 编码高字数位，64 分片读写锁 `stripe_index` 在大对象上保持不变。
- **容器容量提升**：`List` 最大容量升至 `67,108,864` 元素；内建 String-keyed `Map` 最大容量从 `8,192` 升至 `16,777,216` 桶（`67,108,864` words）。

### 2. `PayloadRole::ExternalRc`（Immix 薄壳 + 堆外原子 RC + 压力记账）
- **脱离固定 GC 堆配额**：多 MiB 乃至百 MiB 级的 `String` 与 `Bytes` 在 MMTk Immix 中仅占 `40 B` 薄壳，不会撑爆 `FixedHeapSize`。
- **防子串滞留（Substring Retention Guard）**：对 `ExternalRc` 大字符串执行切片时，若切片长度 `≤ 8,168 B` 则复制为紧凑的 Immix 内联字符串；若切片长度 `> 8,168 B` 且占原缓冲容量 `≥ 1/4` 则通过 `ExternalRcHandle::clone` 进行 $O(1)$ 零拷贝共享，否则分配匹配大小的新 `ExternalRcHandle`，避免小切片长期钉住百兆父缓冲。
- **外部内存压力触发器**：`EXTERNAL_UNCOLLECTED_BYTES` 累计自上次 GC 以来的堆外分配字节数；一旦达到阈值（`clamp(heap_size / 2, 8 MiB, 256 MiB)`），自动置位 `GC_FLAGS.gc_requested`，在下一个指令安全点触发回收并清扫不可达薄壳，将堆外内存即时归还操作系统。
