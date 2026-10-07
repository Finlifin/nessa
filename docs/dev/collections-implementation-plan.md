# List / Map 实现准备（2026-10-07）

## 当前状态：动态 List 与字符串键 Map 已落实

动态 Any 元素 List 已贯穿类型角色、字面量、受检索引/更新、native/std 方法、
单变参打包、显示、GC 和自包含归档。具体行为与限制见
[implementation-progress.md](implementation-progress.md#动态-list单变参与归档兼容2026-10-07)。
字符串键、Any值的Map现已贯穿构造、受检索引/更新、native/std方法、显示、GC
和自包含归档；支持碰撞、tombstone、扩容及8192桶容量边界。缺失get/remove
返回null，contains区分存储null。List和Map的单次next快照迭代已接入，Map的
Item准确为(String,Any)，另提供keys/values/entries快照；详见builtin-types。
泛型元素/键值约束、语言Hash/Eq键分派、
双变参及匿名Object混合容器仍未实现。具体Map验收见implementation-progress.md。

以下保留实施前的只读调查与建议；其中“当前”“缺口”指调查当时，不代表本轮
完成后的状态。实际采用稳定 List/Buffer TypeId 与 TPOL1 原索引恢复方案，
初版没有新增Intrinsic；本轮追加独立Map/MapBuffer角色，仍未采用备选
CollectionTypes归档表，未改旧List或typepool前缀身份。

## 设计依据与实际缺口

依据是 `grammar/literals.md`、`grammar/expressions.md`、`type-system/builtin-types.md`、
`type-system/type-id.md`、`dev/architecture.md`、`runtime-representation/data-type-layout.md`、
`memory-management/intro.md` 和 `nessa-bytecode/archive.md`。以下路径相对 `docs/`。
旧布局文档中的外部裸数组指针不是已经可用的实现；当前 GC 不会自动追踪 Rust Vec。

| 阶段 | 已有代码 | 本轮调查确认的缺口 |
| --- | --- | --- |
| Parser / AST | `try_list` 产生 `ListOf`，多子节点保存元素；`Object` 保存属性和普通元素 | 不是独立 Map 字面量契约；索引语法也不能由存在 opcode 推断为已接通 |
| Resolution / NIR | `NirExpr::IndexAccess` 已定义；codegen 能发 LoadIndex | ListOf 没有专门类型与 lowering，落入 NIR 默认 Unit；没有实际 NewList/NewMap 构造路径 |
| VM | NewList/NewMap、LoadIndex/StoreIndex 分派存在 | 分配/读取返回 Unit，写入无操作；artifact validator 明确拒绝这四类占位 opcode |
| Native | `LIST_INIT = 100`，名字 `__list_init`，启动时注册 | 精确零参数 ABI，实际返回 Unit；不能改成“接收元素的 variadic 构造”而沿用旧 ABI |
| std | 七个实际嵌入源码文件提供 native adapter 与 prelude | 无 List/Map 公共 API，无 alloc 库；`__list_init` 未被 std 源码包装 |
| TypePool | 现有 Intrinsic、Struct、Tuple 等种类和完整 TPOL1 exact-index 恢复 | 无 List/Map intrinsic 或 collection role，不应靠类型名称或固定用户类型索引识别 |
| GC / BuiltinCtx | payload 按 TaggedValue 扫描；native `arg` 根保留到 context 退出 | 没有任意对象分配/临时根公开 API，也没有集合 grow 和 backing-buffer 的安全发布路径 |

`__list_init` 当前注册可调用不代表已经能创建列表。`[]`、`[1, 2]` 的 parser 接受
不代表运行值正确；实施前先补回归，禁止保留 Unit 占位而宣布 collections 可用。

## 推荐的类型与归档兼容策略

保留 `Intrinsic::COUNT = 23`：现有 intrinsic 索引为0–22，well-known traits 占23–30，
canonical null 占31。直接在 Intrinsic 尾部加 List/Map 也会改变后两组索引，从而破坏
TPOL1 旧归档。将新种类插进枚举中间更不能接受。当前归档必须恢复原始索引，不能
通过重新 intern 或把旧用户类型后移来“兼容”。

首个切片推荐复用现有 `TypeKind::Struct`，为 List、Map 和私有 value buffer 赋予
明确、版本化的 engine-reserved TypeId。身份编码必须是与 StrId 无关的确定常量，
不复用 intrinsic 或 trait 命名空间，也不使用 TypeId::ZERO。布局改变时换身份版本。
实际编号在实施时定义并审查；本文只约定分配规则，不虚构已经注册的编号。

- 新编译池在现有 intrinsic/trait/null 前缀之后注册这些描述；不改变既有前缀。
- `catalog_register_intrinsic_types` 当前只注册 intrinsic，需另注册集合名字；源码
  类型别名仍经 trusted std 的 builtin view 暴露，不向用户 root 注入新名称。
- VM 根据当前池的 `lookup_by_id` 加 canonical 检查取得集合 role，绝不把默认新池
  的追加索引当作加载池索引。校验 role 的种类、字段类型/顺序、大小、对齐和 buffer
  描述后才能解释 payload；同名用户 Struct 不取得 native 集合身份。
- TPOL1 已能保存 Struct、TypeId、方法和 exact indices，因而无需新增 TypeKind tag
  或改 TPOL revision。旧池不包含集合身份时，普通旧产物仍可执行；执行集合 opcode
  应明确报缺少集合描述。不得向已经恢复的旧池前缀插入新描述。
- 首版支持 Any 元素；`List[i64]` / `Map[String, i64]` 的完整静态参数化种类不在此
  切片。以后新增 TypeKind 需要独立 tag/revision 和旧格式读取策略。

备选是显式保存 `CollectionTypes { list, map, buffer }` role 表。此方案不依赖保留
TypeId，但需修改 CompiledArtifact、NSAM metadata revision 和所有安装/验证路径。
TPOL1 仍可保持不变；旧 NSAM1 映射为无集合 role，不能从类型名字猜测 role。两种
方案只能选一种贯穿，不应同时保留两套随意查找逻辑。

`__list_init` 由 Unit 占位改为真实 List 会改变可观察的 native 返回契约。新 API 的
ID/名字 manifest 与 builtin ABI revision 必须一并明确：不能只保留 revision1，
让旧产物中的 Unit-returning adapter 在加载后突然变成不同含义。建议提升 builtin
ABI revision 并明确拒绝不兼容的旧 revision；若要求旧普通产物仍可执行，可以
显式接受未引用ID100的旧 revision1 manifest，拒绝旧revision中引用ID100的产物。
这需要 driver 的明确兼容矩阵，当前单一revision相等检查不会自动实现它。不能
将旧 Unit 占位继续当作成功。TypePool/TPOL的向后解码兼容与native ABI的执行兼容
是两层不同检查；新 builtin ID 追加，不重排现有 ID。

## 与现有 GC 匹配的首版布局

全部使用 GC 分配、8字节对齐、16字节 ObjectHeader。ObjectHeader 的 payload count
是 u16，单个对象最多65535个 payload words。现有 scanner 对普通非 intrinsic
对象逐 word 扫描；只有 String、数值、Continuation 和 Closure 有专门跳过规则。

| 对象 | payload words | 表示 |
| --- | --- | --- |
| List wrapper | 3 | `len: TaggedUInt`、`capacity: TaggedUInt`、`buffer: TaggedValue` |
| List buffer | capacity | 每个 slot 一个 TaggedValue，未使用 slot 初始化 Unit |
| Map wrapper | 3 | `len: TaggedUInt`、`capacity: TaggedUInt`、`buffer: TaggedValue` |
| Map buffer | 4 × capacity | 每个 bucket 为 `state: TaggedUInt`、`hash: TaggedUInt`、`key`、`value` |

空容器可用 null buffer，len/capacity 均为0；第一次写入分配 buffer。Map state 使用
明确枚举编码（空、占用、删除），不是 key=null 哨兵；所有未占用 key/value 初始化
Unit。Hash 存储限定57位的无符号立即值，避免超过立即范围后默默截断，也避免裸
64位 hash 恰好被 GC 视为堆指针。首版可使用确定的字符串 hash，处理 collision；
此选择不是语言 Hash trait 的完整实现。

wrapper 的 Struct 描述列出这三个内部字段；value buffer 使用独立私有 Struct role
作为变长 TaggedValue 区，描述不伪造每个 capacity 的静态字段类型。native helper
以 ObjectHeader 的受检 payload count 解释 buffer。该 opaque role 是受限实现约定，
不是新增的一般用户 Struct 变长布局；需要同步说明并禁止普通对象 API 操作它。

不要按旧设计图直接写 raw usize、外部 Vec 指针或 ctrl/entries 裸指针进普通对象：
当前 scanner 会把低 tag 为000的对齐整数当成引用，且不会管理外部 Vec 的长度、
元素根或释放。所有元数据应 tagged；buffer 必须是 GC 对象引用。全部 slots 通用
扫描即可，不需要让 collector 根据某个 VM 的动态 TypeIndex 找 TypePool。多 VM
加载池可能给同一 role 不同 TypeIndex，故不能加入全局固定索引的 scan 特判。

List 的首版单 buffer 容量明确限65535；Map 4-word bucket 的容量限16383，若使用
2的幂容量则上限8192。grow 的 checked arithmetic、负数/过大请求、负载率和下一
容量都在分配前检查，超限报明确容量错误。这里是首版受检限制，不是无限动态数组
支持；突破此限制需分块/树式 buffer 或新的对象计数格式与 GC 设计。

所有 native 访问先验证 wrapper 身份、payload_words==3、tagged len/capacity、
len<=capacity、buffer 身份及实际 payload words。Map 再检查 bucket state、已用数量
与 key 类型。不能仅凭对象名字、传入指针或头部类型 ID 就 unsafe 读任意长度。
普通 LoadField/StoreField/NewObject 不能绕过集合内部不变量：集合角色只允许受检
collection helper 构造/访问；普通对象指令遇这些 role 应明确拒绝。只标源码字段
私有不够，当前 FieldInfo 没有字段可见性，且 StoreField 只按头部实际 words 检查。
这也避免为每次操作遍历整个 buffer 才能证明对象未被任意字段写入破坏。

## 分配、rooting 与 grow 接口

核心机制放 interpreter 的独立 collections 模块；runtime 仅定义布局常量/受检 view
所需数据契约，gc 继续负责通用分配/扫描。native 层不能获得任意 Heap 或 VmRoots。

建议 VM 内部接口采用 `Result<TaggedValue, VmError>` 或明确 value/error：创建、len、
list get/set/push/pop、map get/set/remove。wrapper 的身份在 grow 中保持不变，更新
buffer 引用并复制活跃元素；旧 buffer 失去引用后由 GC 回收，不手工释放 GC 内存。
List 越界读写明确错误；Map 缺失读取和删除返回明确 optional 结果。字符串键首版
不接受任意 Any 键，错误键类型必须报错，不能退化为指针/显示文本比较。

根发布顺序必须是：

1. 把 receiver、key/value 和构造元素留在 caller slots/registers 或临时根域。
2. 分配 wrapper 并完整初始化为有效空对象，立即登记根；再分配 buffer。
3. buffer 所有 slot 初始化 Unit，并在下一次分配/操作域退出前登记根或发布给
   wrapper。填充只读取已登记的元素，完成后再发布 len/capacity。
4. grow 期间旧 wrapper/buffer/待插入值保持根；新 buffer 分配后重新取得可能移动
   的对象地址，复制元素，最后一次性更新 wrapper buffer/capacity/len。
5. 失败不改变原容器内容和长度；临时新对象按正常 GC 释放，不留下半初始化槽。

当前 `BuiltinCtx::arg` 把交给 native 的堆值保留到 context 退出；即使 r0 被返回值
覆盖仍有根。`set_return` 对没有临时根记录的 heap 值返回 TypeError。
`return_string` / `return_number` 已提供分配后直接发布的模式，`collect_garbage`
可用于真实收集测试。现无公开 arbitrary-object allocator 或 temporary-root handle。
应新增语义明确的 `return_empty_list` / `return_empty_map` 等受检 façade，或提供
有限的 scoped root handle 让同一 helper 完成多步分配；不能绕过 set_return 或把
heap 值先放进普通 Rust Vec 再假定它有根。通用 HostRoot 仍是另一项 API 工作。

当前 plan 的主要实际验证是 Immix 与 pinned/tagged roots。未来移动 collector 下，
复制 TaggedValue 到普通局部变量不等于 root handle：allocation 后必须从可更新的
根槽重新读取，不能继续用旧裸地址。不得让新 collections 代码预设所有未来 GC
都是非移动；至少把潜在跨分配借用限制在 VM helper 内并说明支持的 collector。

## 构造、指令与 std ABI

保留现有 NewList `size=imm12` 含义：分配指定初始长度、元素为 Unit，不把 size
悄悄重解释为容量。NewMap `capacity=imm12` 创建空 Map，len=0；capacity 是受检
预分配请求。dst 是寄存器，base 为未使用字段应为0。较大容量可通过 native API，
不能截断进 imm12。allocator 和 artifact validator 都检查 role 与范围。

现有 LoadIndex/StoreIndex 文档把 imm12 解释为 index/key register 编号，不是常量
池或字面量索引：只允许0–31并验证 declared register_count；object 在 base，
LoadIndex dst 为结果，StoreIndex dst 为写入值。动态 List 要求精确非负整数索引；
Map 要求字符串键。IndexAccess codegen 当前使用 Reg(index).0 写 imm12，与该方向
相符；StoreIndex AST/NIR 赋值路径仍需完整接通。修改 validator 接受这些 opcode
之前，VM 错误路径与源码 lowering 必须同时完成。

`__list_init` 保持零参数调用，返回真实空 List。新 List 字面量按源码顺序求每个
元素并存入 rooted local slots，再创建和填充；不能把任意长度元素挤进32个参数
寄存器，也不能颠倒副作用顺序。建议 NIR 有专门 NewList/StoreIndex 或 build helper
计划；每一步都可在 GC safe point 保持中间结果。Map 构造同理先保存 key/value。

typed std API 至少包含 List/Map 类型别名、空构造、len、list get/set/push/pop、map
get/set/remove；按实际 builtin ABI 明确参数与返回值，不附造假的泛型签名。首版
可在现有 std 模块接入并更新 driver 嵌入源码清单，或新增 std.collections 并补真实
SourceMap/模块/use/prelude 加载。仅放文件到 library 不代表会加载。

`{ name: value }` 是匿名 Object 设计，不能直接当通用 Map 来实现并宣布支持混合
Object。先提供显式 Map API；以后 Object 的 List+Map 组合需自己的完整语义和布局。

## 实施与验收顺序

1. 先锁定 List/Map 错误、缺失键/越界行为、首次集合 API 和 ABI revision 策略；
   对仍不支持的 ListOf/collection AST 报明确诊断，停止 Unit 占位成功路径。
2. 注册角色描述/身份，补 TypePool snapshot、旧 TPOL1、alias 与新角色校验回归。
3. 实现受检 layout、空构造及 rooting façade；真实 GC 覆盖仅容器持有的字符串。
4. 完成 list get/set/push/pop 与 grow，覆盖容量边界、失败原值、别名共享和次序。
5. 完成字符串键 Map、collision/tombstone/grow/remove；错误键类型明确拒绝。
6. 接通源码类型、NIR/codegen、std、index 赋值和 artifact validator；不要先放开
   loader 再留下 VM 占位。同步增加每个 opcode 的格式/寄存器/容量拒绝测试。
7. 完成独立 CLI 归档往返：删除源码后执行、加载前扰动 StrId、旧普通产物兼容、
   集合身份缺失/损坏 metadata/ABI 不兼容拒绝。归档保存的是构造代码与类型元数据，
   不是正在运行的 heap 容器快照；不要新增可序列化裸堆指针常量。

必须覆盖空/嵌套/混合 heap 与 immediate 元素、共享 mutation、越界及错误类型、
删除后引用释放、连续 grow+真实 GC、handler/continuation 暂停帧中容器存活，以及
新容器只由 global 持有的根。原生集合 hash 的 collision fixture 应确定，不依赖
系统随机种子或测试执行顺序；平台/plan 差异明确限定。性能优化在语义闭环之后，
Robin Hood/Swiss Table 选择不应替代 correctness/rooting 验收。
