# NSBC Archive 文件格式

NSBC Archive (`.nsbc`) 是 Nessa 的编译产物容器。当前完整产物是已经合并依赖并编排
初始化的自包含编译单元，可在新进程中独立加载执行；跨包链接和 TypeIndex 重定位
尚未实现。下文区分当前可执行格式与保留的旧低层格式，不能按文件后缀假定可执行。

`nsbc_io::write_artifact/read_artifact` 读写完整 `CompiledArtifact`：CodegenOutput、
exact-index TypePool、入口、builtin ABI revision 和 ID/名字 manifest。
旧 `write_archive` 仅保存 CODE/CONSTANTS/占位 STACK_MAPS，不具有执行所需元数据；
遇非空 global schema 返回 Unsupported，完整 loader 拒绝缺少完整 METADATA 的文件。

## 文件结构总览

```
┌──────────────────────────────────────────────────┐
│ File Header         (60 bytes, 固定大小)          │
├──────────────────────────────────────────────────┤
│ Section Table       (N × 32 bytes)               │
├──────────────────────────────────────────────────┤
│ Section 0: CODE                                  │
│   函数字节码流，按 FuncId 索引                     │
├──────────────────────────────────────────────────┤
│ Section 1: METADATA                              │
│   类型池 + 函数签名 + globals + 入口 + builtin      │
├──────────────────────────────────────────────────┤
│ Section 2: STACK_MAPS                            │
│   每函数的 safe-point PC（当前 TaggedValue 扫描）   │
├──────────────────────────────────────────────────┤
│ Section 3: CONSTANTS                             │
│   常量池 (数值、字符串字面量)                      │
├──────────────────────────────────────────────────┤
│ Section 4: DEBUG_INFO  (可剥离)                   │
│   源码映射、行号表                                │
├──────────────────────────────────────────────────┤
│ Section 5: IMPORTS                               │
│   外部依赖声明 (domain/name + version)            │
├──────────────────────────────────────────────────┤
│ Section 6: EXPORTS                               │
│   导出符号表                                      │
├──────────────────────────────────────────────────┤
│ Section 7: WASM  (可选)                           │
│   嵌入的 WASM 模块                                │
└──────────────────────────────────────────────────┘
```

## File Header

```
FileHeader (60 bytes):
  magic:          [4 bytes]   "NSBC"                            → 魔数
  version:        [4 bytes]   u32（当前 = 4，受检Error envelope）    → archive 格式版本
  checksum:       [32 bytes]  SHA-256 of (header 以外所有数据)    → 完整性校验
  target_arch:    [2 bytes]   0=Any 1=X86_64 2=ARM64 3=RV64     → 目标架构
  target_os:      [2 bytes]   0=Any 1=Linux 2=Darwin 3=Win      → 目标 OS
  flags:          [4 bytes]   bit0=debug bit1=compress ...       → 标志位
  section_count:  [4 bytes]   u32                                → section 数量
  section_table:  [8 bytes]   u64                                → section table 在文件内的偏移
```

所有多字节整数均为 little-endian。header 实际写入 60 字节。
当前writer生成VERSION4；reader兼容普通VERSION3产物。Error的新执行能力
要求VERSION4，不能仅通过改header版本把新布局混入旧格式。下文较早阶段的
NSBC3记录保留其当时的兼容性背景；TYPE_METADATA仍使用TPOL11。

`target_arch` 和 `target_os` 为 `Any(0)` 时表示平台无关字节码；完整 loader 要求非零
目标与当前执行平台匹配。writer 当前写 portable 目标。

writer 对 header 之后的全部字节计算 SHA-256，包含 section table 和 alignment
padding；reader 校验非零 checksum。低层容器仍允许 zero checksum 以检查旧文件，
但完整 loader 拒绝 zero checksum。checksum 用于发现损坏，不是认证签名；它不覆盖
header，目标、版本、flags 等 header 字段仍需分别校验。

读写总大小限额为 `MAX_ARCHIVE_SIZE = 64 MiB`，section 数量上限 1024。
header 当前只允许 debug bit0；压缩 bit1 与未知 flags 明确拒绝。完整执行 reader
当前只接收 CODE、METADATA、STACK_MAPS、CONSTANTS 四种 section，其他 section
类型仍是设计预留，并不表示实现了 debug、imports/exports 或 WASM 装载。

## Section Table

Section table 位于 `section_table` 指定的偏移处，每个 entry 32 bytes：

```
SectionEntry (32 bytes):
  name_idx:    [4 bytes]   u32  → 当前完整格式必须为 0（未命名）
  type:        [4 bytes]   u32  → section 类型枚举
  offset:      [8 bytes]   u64  → 该 section 数据在文件内的偏移
  size:        [8 bytes]   u64  → 数据大小 (bytes)
  alignment:   [4 bytes]   u32  → 对齐要求
  flags:       [4 bytes]   u32  → 标志位 (bit0=compressed, bit1=strippable)
```

Section type 枚举值：

| 值 | 名称 | 说明 |
|----|------|------|
| 0 | CODE | 字节码指令流 |
| 1 | METADATA | 类型表、方法表、字段表、字符串池 |
| 2 | STACK_MAPS | Safe-point 位图 + deopt 信息 |
| 3 | CONSTANTS | 常量池 |
| 4 | DEBUG_INFO | 源码映射（可剥离） |
| 5 | IMPORTS | 外部包依赖声明 |
| 6 | EXPORTS | 导出符号表 |
| 7 | WASM | 嵌入 WASM 模块 |

## 完整格式标识与 CODE Section

外层 VERSION 仍为 3。完整 METADATA 以 `NSAM` magic 和独立 revision 1 开头，
这是解释以下 CODE/STACK_MAPS 布局的必要条件。缺少 magic、未知 revision 或缺少
任一必需 section 都不能执行；不得将旧 CODE 当成这个布局静默解释。

```
CODE:
  func_count: u32
  entries[func_count]:
    func_id: u32
    code_offset: u32       # 相对于整个 CODE section 起始
    code_size: u32         # 字节数，必须为 4 的倍数
    register_count: u8
    param_count: u8        # 包括 captures
    flags: u16            # bit1=is_closure；其他位当前拒绝
  instructions: u32[]
```

每个 entry 16 字节；首个函数 offset 至少为 `4 + func_count * 16`。
函数字节区域必须完整覆盖表之后的数据，不重叠、不留未声明字节，保持指令对齐。
FuncId 必须从 0 连续；entry、签名、stack-map 元数据必须对应同一组函数。

旧 `write_archive` 的 CODE offset 相对于 code blob，closure flag 使用 bit0。
它属于不完整低层输出，不能由完整 loader 按新布局执行。

## METADATA Section（NSAM revision 4/5/6，兼容 revision 1/2/3）

`string` 为 `u32` UTF-8 字节数加字节，`blob` 为 `u32` 字节数加字节，
`list` 为 `u32` 元素数加元素。名称采用内联 UTF-8，不保存进程 StrId。

```
METADATA:
  magic: [u8;4] = "NSAM"
  revision: u32 = 4/5/6 # 有Display owner时为6，否则有TraitSelf时为5
  root_scan_mode: u32 = 0  # 扫描全部 TaggedValue 根，非精确 bitmap
  builtin_abi_version: u32
  entry: u32              # 0xFFFFFFFF=None；可能是 __startup
  type_pool: blob         # 有IterationStep结构来源时TPOL8；否则默认模板TPOL7/关联数据TPOL6/普通TPOL5；兼容读取1–8
  globals: list<(type_index:u32, is_mutable:u8)>
  functions: list<(func_id:u32, name:string, function_type:u32)>
  methods: list<(constant_slot:u32, name:string)>
  builtins: list<(id:u32, name:string)>
  scope_coverage: u8      # 0=Calls，1=CallsAndTypes；revision 4 显式保存
  scopes_present: u8
  method_call_scopes: list<(func_id:u32, pc:u32, scope:u32)> # 仅 presence=1
  function_abis: list<(func_id:u32, abi:option<FunctionAbi>)>
  display_owners: list<(func_id:u32, owner:option<u32>)> # 仅revision6，覆盖全部函数

FunctionAbi:
  captures: list<CaptureAbi>
  parameters: list<ParameterAbi>
CaptureAbi: tag:u8，0=Value，1=TraitProof 后接 view:u32
ParameterAbi: tag:u8，0=Value，1=Trait 后接 view:u32，
  revision5/6另允许2=TraitSelf 后接view:u32（具体Self签名，proof/data入口）
option<FunctionAbi>: presence:u8，然后仅 presence=1 时保存 FunctionAbi
```

bool 只接受 0/1。functions 不接受重复 ID。globals 按原 GlobalId 顺序保存 schema，
不保存执行后的值或 initialized 状态。第一次 store 初始化 const，此后不可重写。
入口直接使用保存的 FuncId，不按名字重新选择 main，也不再次规划模块初始化 DAG。
Function 签名不包含 captures；内部 handler/initializer 可使用 INVALID，源码函数
和 typed native adapter 的真实签名用于 reflection 与动态类型检查。

源码函数保存显式 FunctionAbi：capture 为物理入口前缀，parameters 表示用户
逻辑参数，不以总参数数减签名参数数推断 captures。Value 占一个入口槽；Trait
占 proof、data 两槽，proof 在 data 前；capture 项各占一个环境槽，TraitProof
必须紧跟其 Value 数据槽。描述符
的物理数量必须匹配 CODE.param_count，逻辑数量匹配有效 Function 签名，普通
函数不得含 captures。ABI table 必须精确覆盖所有函数，无重复或未知 ID。
计数、标签、presence、view 类型和分配预算受检。裸 trait 参数及其闭包 capture
已执行 proof/data 检查。默认方法 adapter 的逻辑 Self 为 concrete implementor，
对应参数用 TraitSelf 描述隐藏 proof/data；validator 要求具体签名和相应实现
table，VM 检查数据与声明 concrete 类型一致。新版本5仅增加此参数标签；
含关联声明的内部 TraitSelf 及闭包证明要求所选根实现具有完整、精确的具体关联
绑定；投影保留原根 table 的方法覆盖。参数及调用签名按该实现 trait 和 scope
实例化，关联 Item 使用普通 Value，即使它与 concrete Self 类型相同。
bare Trait 参数的关联视图仍拒绝，不据此引入用户动态关联 trait carrier。
这些检查沿用现有描述符、绑定及指令，未新增归档标签。
revision4仍拒绝tag2。无Display owner且无TraitSelf的Some仍写4，旧None仍按
1/2/3保存，reader兼容1–6。trait返回与嵌套存储carrier尚未完成。

NSAM6在完整ABI表后追加Display owner表。表计数必须等于函数表计数，ID唯一
且精确覆盖；owner presence只能为0/1，Some随后保存TypeIndex u32。owner需
对应可信bootstrap Display的全局派生实现及实际FuncId，接收者为具体聚合类型，
签名为fn(Self)->String、无capture且入口为Value。错误身份、类型、ABI、表覆盖、
截断及降版后遗留尾部均拒绝。旧NSAM1–5读入owner全为None，重写不补造。
writer仅在存在Some owner时选择6；NSBC3及TPOL修订独立于此扩展。

builtin ABI7新增123 __display_quote、124 __display_enter_tuple、125
__display_exit_tuple。旧ABI6仅在全部import ID小于123时允许；旧5小于122、
4小于121、3小于120、2小于110、1小于100的限制保持。所有import精确检查
ID与名称，不能声称旧ABI却导入新helper。旧struct派生120的格式与行为保持。

builtin ABI8 追加126 `__map_keys`，调用契约为一个Map参数、返回键引用的独立
List快照。旧ABI7只接受ID小于126；1–6的原阈值不变。安装新helper前必须具备
Map、MapBuffer、List和Buffer四个受检角色。旧归档不补造新native能力；Map和
List的对象布局、TPOL、NSAM及外层NSBC不因本helper改变。

新证明指令要求显式 ABI。A-type 0x6A TraitProof、0x6B TraitAssert、0x6C
TraitProject 的 view 是12-bit TypeIndex；TraitAssert 的旧 dst 保存待检查 data。
C-type 0x92 TraitCall 使用5-bit物理参数数、5-bit proof寄存器和12-bit接口槽；
r0 是 receiver data，额外参数按逻辑类型展开，proof寄存器位于参数区之后。
0x93 CallIndirectProof 的数量是用户物理参数数，不含 captures；旧 CallIndirect
仍按逻辑参数处理。view/slot 不是常量或 StrId，不参与常量/字符串重定位。
validator 检查 view/schema、布局、寄存器及槽的可能范围；VM 根据实际 handle
检查精确 view/slot 和 data 类型，静态验证不证明完整 proof 数据流。handle
是独立 immediate，不是 UInt/Symbol 常量，也不会持久化到文件。旧 reader
拒绝未知 opcode，旧1–3的入口解释不变。

builtin manifest 只含稳定 ID/名字，不含函数指针。完整 bytecode validator 检查重复
ID/名字和实际 CallBuiltin 的声明覆盖；执行前 driver 校验 runtime ABI revision、
ID/名字。签名来自源码 adapter，不向 builtin 注册表添加静态签名。

新编译结果使用完整词法上下文表（包括空表）及显式 ABI，writer 按上述规则写 NSAM4/5/6。表按真实指令
PC 保存词法 scope，覆盖 CallIndirect、CallMethod、CallMethodFar、TraitProof、
TraitCall、CallIndirectProof，以及 TraitAssert、TypeCheck、
TypeCast、TypeCastSafe、TypeAssert、StoreGlobal/StoreGlobalWide、LoadField/StoreField、
NewEnum 和 EnumField。后六种指令内部会检查声明类型，不能遗漏它们的上下文。
不允许重复位置、未知函数、未覆盖指令 PC 或无效 scope。默认值采用声明侧表达式
scope，闭包及 continuation 恢复使用它们自身的函数和 PC，不能借动态 caller
或 handler 的权限。方法名和常量搬迁保持指令 word 数量，表内 PC 不变。

旧 NSAM1 没有该表，reader 保留 None，重新写入仍为 NSAM1；不能用空表冒充
完整新上下文。无图的旧产物可加载，但 LegacyUnknown 方法不能授权，需重编译；
普通函数、闭包和明确的 intrinsic 公共协议仍按各自 ABI 处理。含词法图且执行
动态调用的产物缺完整表时在加载前拒绝。TPOL5 与 NSAM revision 独立，纯直接
调用无需动态表时仍可使用旧 wrapper；不是偷偷更改旧字段的解释。

NSAM2/3 的 scope 表分别承诺 Calls/CallsAndTypes；NSAM1–3 没有 FunctionAbi，
reader 保留 None，重写全部 None 的产物仍按原 scope 契约写1/2/3。存在 Some
时写4，混合产物逐函数保留 None/Some；scope 表的 None 与 Some(empty) 也区分，
不从 revision4 推测完整类型上下文。旧 reader 明确拒绝未知 revision4。

### TypePool payload（TPOL revision 5/6/7/8/9/10）

```
TPOL:
  magic: [u8;4] = "TPOL"
  revision: u32 = 5 | 6 | 7 | 8 | 9 | 10
  types: list<TypeInfo>            # 顺序就是 TypeIndex
  structural_types: list<u32>      # 来源记录，不从 kind 猜测
  methods: list<list<MethodSlot>>  # 与 types 平行
  trait_impls: list<(trait_type:u32, implementor:u32, visible_scope:option<u32>, methods:list<MethodSlot>)>
  vtables: list<(trait_type:u32, implementor:u32, visible_scope:option<u32>, entries:list<u32>)>
  well_known: u32[8]               # Display,Hash,Eq,Ord,PartialEq,PartialOrd,Iterator,IntoIterator
  null_type: u32
  scopes: list<ScopeContext>       # 顺序就是归档局部 scope ID
  trait_schemas: list<(trait_type:u32, slots:list<(trait_owner:u32, name:string, signature:option<TraitMethodSignature>)>)>
  associated_bindings: list<AssociatedTypeBinding> # revision6以上
  associated_defaults: list<(trait_owner:u32,name:string,expression:AssociatedTypeExpr)> # revision7以上

TypeInfo:
  type_id_hi: u64
  type_id_lo: u64
  size: u32
  align: u32
  kind_tag: u8
  kind_payload: ...

FieldInfo: name:string, type_index:u32, has_default:u8, offset:u32
MethodSlot: name:string, func_id:u32, trait_impl:option<u32>, visible_scope:option<u32>, access:MethodAccess
MethodAccess: tag:u8，0=LegacyUnknown，1=Public，2=Package后接package:u32，3=Private后接scope:u32
ScopeContext: parent:option<u32>, package:u32, assoc_type:option<u32>
TraitMethodSignature: declaration:u32, self_paths:list<list<TraitTypeStep>>, parameter_kinds:list<u8>
                      associated_paths:list<(trait_owner:u32,name:string,path:list<TraitTypeStep>)> # revision6以上
AssociatedTypeBinding: implementor:u32, trait_type:u32, visible_scope:option<u32>, trait_owner:u32, name:string, value:u32
TraitTypeStep: tag:u8，0=Parameter后接u32，1=Return，2=TupleElement后接u32，
               3=OptionalInner，4=ErrorInner，5=ErrorMember后接u32，
               6=EffectInner，7=EffectMember后接u32
parameter kind: 0=Receiver，1=Required，2=Optional，3=ListVariadic，4=MapVariadic
option<u32>: presence:u8，然后仅在 presence=1 时保存 u32
```

| kind tag | 类型 | payload |
| --- | --- | --- |
| 0 | Intrinsic | 固定 intrinsic 编号 u8 |
| 1 | Struct | name:string, fields:list<FieldInfo> |
| 2 | Enum | name:string, variants:list<(name:string, tag:u32, fields:list<FieldInfo>)> |
| 3 | Typealias | name:string, target:u32 |
| 4 | Newtype | name:string, inner:u32 |
| 5 | Tuple | elements:list<u32> |
| 6 | Function | params:list<u32>, ret:u32 |
| 7 | Effect | params:list<u32>, ret:u32, is_async:u8 |
| 8 | Optional | inner:u32 |
| 9 | ErrorQualified | errors:list<u32>, inner:u32 |
| 10 | EffectQualified | effects:list<u32>, inner:u32 |
| 11 | Module | name:string |
| 12 | Trait | name:string, parents:list<u32>, assoc_types:list<(name:string, default:u32)> |
| 13 | AssociatedType（TPOL7以上） | trait_owner:u32, name:string |
| 14 | IterationStepTemplate（TPOL9以上） | item:u32 |

snapshot/restore 保持 exact-index，不能通过结构去重重建索引。structural_types 区分
结构签名与同形的名义 effect 声明。加载时重新 intern 名称并重建查询缓存，验证
intrinsic 前缀、类型引用、alias/结构/trait 环、派发表、well_known 和 null_type。
TPOL 总 payload 限额 64 MiB，累计 list 元素数上限 262,144。

TPOL4 追加 trait 方法 schema，槽保存声明 trait owner 与名称。父槽先于自己的
声明，按名称去重，继承的同名槽可由 child record 的方法覆盖；描述同时保留
声明 owner 与实际 implementation trait，不能把二者混为同一身份。bootstrap
使用固定接口，不能让第一个 implementation 自行定义同 trait 的槽顺序。
恢复时检查 schema owner/名字/父顺序及 table 每个槽的实际函数 ID；已知的其它
函数也不能替换接口槽。派生函数发布同步更新所有确实继承该实现的 table 槽。

TPOL1–3 没有 schema，reader 保留空列表，重新保存为4仍为空；不能从函数 ID
猜接口并授权 evidence 调用。TPOL5 在槽身份后保存可选的接口声明签名、参数类别和真实 Self 的结构路径。
声明必须是 Function，路径按类型构造检查、终点属于声明 trait；重复/重叠/越界
路径拒绝，最大深度256。显式写出的 trait 与仅 alias 到该 trait 不被当作 Self。
Self 在 Tuple/Optional/嵌套 Function 或限定类型中按真实路径替换为 implementor，
其它类型精确比较 canonical 结构，不用 Any 或数值宽容掩盖接口差异。loader 对
有签名的槽校验实际目标 function_type，不用 physical param_count 推断接口。

旧1–4缺少签名，升级到5仍保留 None；没有声明的 bootstrap 槽当前亦为 None，
不能伪称全部接口均已证明。裸 trait 参数的 physical proof ABI 和 capture
传输已实现；默认方法具体 adapter 已按独立 fresh FuncId 及 TraitSelf 接通，
冻结父视图投影保留 Child 覆盖。缺签名 bootstrap 槽及 trait 返回/存储仍待完成。该签名元数据本身不改变旧入口解释。
TPOL5 的签名字段本身不改变旧入口；显式 FunctionAbi 由独立的 NSAM4/5 保存。
TPOL6保存源关联类型路径及精确实现绑定。关联路径和Self路径独立，显式Any不因
某个Item默认值为Any而被替换；名字以UTF-8保存，读取后重新intern。绑定身份包含
implementor、实现trait、visible_scope、声明trait owner及关联名，不由当前调用
作用域重新猜测。缺少对应实现、重复绑定、未知声明、无效value或路径均拒绝。
无关联路径/绑定的新产物仍写revision5；旧1–5读取后两项为空，不自动补关联协议。
revision6不新增opcode、intrinsic、builtin导入或NSAM布局，旧reader拒绝未知revision。
外层 NSBC3 保持不变。builtin ABI4 增加 ID120 __derived_display，供新源码
派生 Display 的普通有类型 wrapper 使用。旧 ABI3 仅在 imports 全部小于120
时兼容，ABI2 小于110、ABI1 小于100的限制保持。所有 ID/name 精确核对。

trait record/vtable 的身份是 canonical (implementor, trait, visible_scope)，
同一作用域重复拒绝，不同作用域可共存；vtable 必须对应完全相同的 record scope。
None 表示全局实现，Some 表示词法扩展；旧全局查询 API 不返回 scoped record。

scope parent 必须有效且无环，每个 package 只有一个词法根；package 是归档局部
身份，不是模块名或进程 StrId。assoc_type 保存 canonical 类型，用于同包内同类型
不同 impl scope 的 private 授权。private/extend scope、包身份及关联类型引用均
受检，访问等级与 extend 可见范围分别保存，不从 visible_scope=None 猜测 public。

reader 保留 TPOL1 的原布局：MethodSlot 没有 access，末尾没有 scopes。旧方法
标为 LegacyUnknown，原 visible_scope ID 保留；缺少图时不能证明授权。新版写入
TPOL5，旧 reader 拒绝未知 revision。TPOL2 仅在方法的 visible_scope 一致时恢复
record scope，旧 vtable 必须能唯一对应一个 record；无方法的旧 record 保留全局含义。
NSAM2 仍只承诺动态调用的上下文，重写不会自动升级为完整类型上下文。缺少
上下文的查询涉及 scoped trait 时显式报 MissingTraitContext，不能借用局部证明。
当前新源码产物使用 NSAM4/5/6 显式入口布局与词法上下文表，
NSBC3 不变；builtin ABI7 的兼容规则见上。VM 在方法分派前消费调用点 scope，按访问等级及
extend 范围过滤候选，可访问的多个同名槽报 AmbiguousMethod；无可访问候选报
MethodAccessDenied，旧权限未知和缺上下文分别报错，不按登记顺序挑首个。

尚未建立稳定身份的 TypeId 为 ZERO，不能以其合并名义类型；保存池内索引不代表
已经实现跨产物身份和链接。名义递归边界与派发表合法 sentinel 遵循 TypePool 的
受检恢复规则。

### 方法名字重定位

写完整 CODE 时，将所有方法调用转为同长度的 CallMethodFar；为每个不同名字建立
专用低位常量槽，磁盘值必须是 `UInt(0)`，methods 表记录该槽与 UTF-8 名字。
加载验证槽的独占使用、声明覆盖和重复项，然后仅将这些槽替换为重新 intern 的
StrId 数值。禁止将其他 UInt 按名字索引猜测并改写。

常量池会重新排序：12 位 closure/Enum descriptor metadata 与专用方法槽优先，随后是 14 位 far-call
metadata，最后是其他常量。所有已知常量 operand 同步搬迁，narrow Load 必要时
原位转为 LoadConstWide。PushCapturingHandler、PushHandlerWide 也搬迁其 metadata
索引；不搬迁指令保留原码字。指令数量、跳转、safepoint PCs 不变。
方法与12-bit metadata 合计超过 4096，或其他搬迁结果超过其 operand 位宽时，
writer 明确返回 Unsupported；不会截断索引或篡改数值常量。重复存档不会不断增加
仅供方法使用的旧常量槽。

## CONSTANTS Section

常量 section 以 little-endian `u32` 的常量数量开头，随后按索引顺序存储
`tag: u8` 与 payload。tag 是归档格式的稳定编号，不依赖 Rust 枚举顺序。

| tag | 常量 | payload |
| --- | --- | --- |
| 0 | Int | 8 字节 little-endian `i64`，二补码 |
| 1 | UInt | 8 字节 little-endian `u64` |
| 2 | Float | 8 字节 little-endian IEEE 754 `f64`，保留原始位模式 |
| 3 | Str | little-endian `u32` 字节数，随后为 UTF-8 字节 |
| 4 | BigInt | little-endian `u32` 字节数，随后为不透明整数 payload |
| 5 | Int128 | 16 字节 little-endian `i128`，二补码 |
| 6 | UInt128 | 16 字节 little-endian `u128` |
| 7 | Type | 4 字节 little-endian `u32`，当前 type pool 的索引；拒绝 INVALID |
| 8 | Enum | `type_index:u32LE`，随后`variant:u32LE`；拒绝INVALID及tag≥2^25 |
| 9 | Char | `scalar:u32LE`；拒绝Unicode代理项及大于U+10FFFF的值 |

128 位 tag 在尚未发布的 v3 格式中补全；既有 tag 0–4 的编号与解释不变。
解析器拒绝未知 tag、截断 payload、非法 UTF-8、与 payload 不符的数量和尾随字节，
不会将 128 位值截断为 64 位。`BigInt` 在此层只保留字节，不承诺 VM 已支持执行。
`Archive::constants()` 解码常量 section；完整 artifact loader 恢复 TypePool 并
验证 Type 索引、所有指令和相关元数据。BigInt 的二进制 payload 可低层往返，但
完整 validator 拒绝其执行，因为 VM 尚未支持。

Type payload 是当前 TypePool 内的引用，不是持久稳定的 TypeId。拒绝 INVALID
只验证编码合法性；可执行加载还必须恢复完整 TypePool，并验证索引属于该 pool。
常量 section 往返不能单独证明 Type 值可在新进程中正确加载。

Enum常量兼作NewEnum/EnumIs的元数据descriptor，必须引用当前pool中的Enum
及存在的variant；只有无载荷variant可直接Load/LoadConstWide，带载荷descriptor
直接加载被validator拒绝。TypePool检查variant tag小于2^25、最大字段数加tag槽
不超过u16 payload容量。不能用普通UInt、Symbol或Type常量替代descriptor。

新增NewEnum(0x67)、EnumIs(0x68)、EnumField(0x69)及MatchFail(0xD4)，细节见
[instruction/intro.md](instruction/intro.md)。NewEnum/EnumIs的12-bit常量索引
加入narrow组并随方法/closure常量排序同步搬迁；EnumField是字段索引，不搬迁。

Enum 实施阶段在 NSBC v3、NSAM1/TPOL1 上追加常量 tag 和 opcode，没有更改布局 revision
或builtin ABI（仍为2）。既有普通常量与指令不重新解释；旧reader遇新tag/opcode
会明确拒绝，不会把Enum编码静默当作旧数值。支持旧普通产物不等于承诺旧错误
Enum表示的运行语义，也不等于完成跨包身份和链接。

## STACK_MAPS Section（NSAM revision 1）

```
STACK_MAPS:
  func_count: u32
  per_func:
    func_id: u32
    safepoint_count: u32
    pcs: u32[safepoint_count]  # 指令索引，不是字节 offset
```

当前 root_scan_mode=0 扫描运行时全部 TaggedValue 根，不消费精确引用 bitmap。
这个 payload 保存实际 safepoint PCs，不包含占位 bitmap/deopt 字段；截断、重复
函数、越界 PC、未匹配的函数元数据和尾随字节均拒绝。
旧低层 writer 仍写零 bitmap/deopt 占位，不能作为已实现精确 stackmap 的证据。
精确 bitmap、deopt/JIT 与相应格式 revision 属于后续切片，详见
[safe-point.md](safe-point.md)。

## 双相格式

| 格式 | 扩展名 | 用途 |
|------|--------|------|
| 二进制 | `.nsbc` | 当前可执行自包含产物；压缩未实现 |
| 文本 | `.nsbc.text` | 设计目标：调试反汇编格式，尚未实现读写 |

文本格式示例：

```
-- hello.nsbc.text
@func main [registers=3, params=0]
  0000: LOAD_CONST  r0, #str:0    -- "hello, world"
  0004: CALL        r1, @println, [r0]
  0008: RETURN_UNIT
```

## 两层组织

- **Archive**: 单个编译单元（一个 `.ns` 源文件或 `__init__` 模块）的产物
- **Package**: 多个 Archive + package.toml 元数据的组合，对应分发/依赖单元


## 当前实现边界（2026-10-07）

nsbc_io 已落实60字节文件头、真实offset与alignment padding，以及受检容器读取。
reader支持重定位section table与乱序section，拒绝越界、重叠、重复、未知kind、
非法alignment、压缩和未知flags；当前限制为64MiB归档与1024个table entries。
此前56字节writer是违反此VERSION 3布局的实现缺陷，没有兼容该错误布局。

完整`write_artifact/read_artifact`已保存TypePool、Function签名、全局schema、
startup入口、builtin manifest及UTF-8名称。读取先验证checksum和执行target，
再验证元数据与指令；CODE所有区间验证后才复制，防止重叠区间放大内存分配。
CLI支持`nessa run file.nsbc`，使用与源码运行相同的受检安装路径。

跨进程回归已在编译进程退出并删除源码后验证初始化、类型反射、闭包、128位
数值、派生方法、effect/continuation和远跳；5000个无关已有StrId不会改变结果。
低层`write_archive`仍不完整，对含globals的输出返回Unsupported；缺少完整
metadata或checksum的legacy容器不能执行。

跨包链接、稳定TypeId、精确stackmap、DEBUG_INFO、文本格式、压缩、WASM和完整
参数绑定ABI仍需落实。当前机器验证不代表整个跨平台设计目标已经完成。

### 基础类型 trait 与字符常量

builtin ABI5新增ID121 `__scalar_eq`，供std源码的标量Eq/PartialEq普通函数调用；
Display仍调用既有ID5 `to_string`。旧ABI4仅当imports全部小于121时可安装，
ABI3<120、ABI2<110和ABI1<100的限制保持。每个import仍校验精确ID/name。
std实现的函数签名、ABI、impl记录和vtable由既有metadata保存，reader不补造
旧文件没有的trait实现。

外层NSBC3、NSAM及TPOL布局revision不变。新增Char tag9保存Unicode标量，
已有tag0–8不重新解释；旧reader遇tag9明确拒绝。解码拒绝代理项、越界标量和
截断payload，VM直接构造字符immediate，无堆分配或GC扫描布局变化。


### Ordering接口与builtin ABI6

新源码Ord继承Eq并包含cmp和四个有默认源码body的关系槽，返回ordinary nominal
Ordering；PartialOrd继承PartialEq并返回?Ordering。签名Self路径、父槽、每个
默认adapter、真实函数ID/ABI及enum variant身份由既有metadata精确保存。
NSBC3、NSAM/TPOL布局不变；不引入Ordering role或新固定intrinsic索引。

builtin ABI6新增ID122 `__scalar_cmp`，native返回?i64，由std源码转换enum。
旧ABI5仅允许imports<122，旧4<121/3<120/2<110/1<100限制不变，manifest
精确校验ID/name。reader不补造旧Ord的父接口、schema或Ordering类型。手构
legacy artifact跨进程执行仍返回整数1，原CmpGt整数指令仍得到true；缺import、
旧ABI声称新native和错误名字均在执行前拒绝。


### 静态关联默认类型模板（TPOL7）

TPOL7在revision6全部字段后追加associated_defaults列表，默认模板按声明trait
owner及名称唯一标识。kind13是该声明的符号叶，trait.assoc_types默认索引必须
指向同owner/name的叶；不能以Any或INVALID冒充未知默认值。模板表达式编码：

| tag | 表达式 | payload |
| --- | --- | --- |
| 0 | Concrete | TypeIndex u32 |
| 1 | SelfType | trait_owner u32 |
| 2 | Binding | trait_owner u32、name string |
| 3 | Optional | inner expression |
| 4 | Tuple | list of expressions |
| 5 | Function | parameter expressions list、return expression |

源Self和显式同trait类型不合并，透明alias展开仍保留来源。恢复检查声明与模板
对应、owner继承关系、关联名字存在、具体类型不含trait视图或符号叶，递归深度
小于256并计入累计item预算。默认引用循环在没有具体实例化时可以保存；具体
实现必须已提供完整非符号绑定，未打断的循环不能发布可执行实现。

writer有模板或符号类型时写7；否则有associated_paths/具体bindings写6，普通
池写5。旧1–6读入templates为空，重写不补造新依赖语义。降版带kind13或新增
尾部拒绝。NSAM6、外层NSBC3及builtin ABI7不因本扩展改变。执行函数签名、
全局槽、Type常量及指令类型操作数不得含符号叶；运行时类型转换、反射和
Optional的null分支也检查此边界。

### 具体带标签迭代结果（TPOL8）

TPOL8 使用原 Enum kind 和 structural_types 字段保存具体 IterationStep(Item)，
没有新增 kind tag 或固定 intrinsic 索引。仅显式结构来源和精确布局同时成立
才识别为该类型：done tag0 无字段；yielded tag1 有一个 value 字段，offset0、
无默认值、具体 Item；旧未finalize池的TypeId为ZERO，finalize后按TPOL11
协议计算完整稳定ID；size/align0。别名 Item 规范化后检查重复
实例。损坏来源、错误布局和重复实例被拒绝，普通同名 nominal Enum 不获得
这一身份。TPOL1–7 拒绝此 Enum 结构来源；不能通过修改 revision 降版保存。
具体源码工厂应用直接保存实例化后的类型及 Enum 常量/构造指令，不保存运行时工厂。
源码删除后独立执行已覆盖准确 Item 与 null 载荷。Iterator单次next协议
与for降低已接入，具体结果仍按此受检结构保存。

### 依赖迭代结果模板（TPOL9）

编译期 `IterationStepTemplate` 使用 kind tag14、Item 类型索引及显式结构来源，
TypeId ZERO、size/align0；Item 必须抽象，循环、非法索引、具体 Item、缺失来源
和重复实例均拒绝。专化后产生 TPOL8 定义的准确 Enum；普通 nominal Enum
不能通过名字或字段形状获得 Item/Self 替换能力。

TraitTypeStep 的 tag8 表示 IterationItem，声明路径只能穿过专用模板；端点、
遗漏、重叠与重复路径沿用受检签名规则。AssociatedTypeExpr 的 tag6 表示
IterationStep，后接一个递归 Item 表达式，沿用深度和数量限制。
这两项与新 kind 在旧 revision 中均拒绝，不通过降版或猜测描述符迁移。

包含模板 kind、路径或默认表达式的池写 TPOL9；仅有具体结果仍写8，
其他池保留7/6/5策略，reader 支持1–9。允许归档保存未使用的声明模板，但
NSBC 验证拒绝模板进入可执行签名、全局、常量和类型操作；Self-only 模板
即使没有 AssociatedType 叶子也拒绝。删除源码后的独立进程执行已覆盖
关联默认结果、作用域不同 Item、嵌套闭包和默认方法体构造/类型值。

### 必须绑定的关联声明（TPOL10）

AssociatedTypeExpr tag7 是 Required，无 payload，表示每个实现必须提供该
声明的准确具体绑定。只允许用于声明顶层，不能嵌入 Optional/Tuple/Function
等默认表达式。缺绑定直接报告错误，不使用 Any 或循环引用作为占位默认值。

含 Required 的池写10，writer/reader 在1–9拒绝该表达式或降版；其他池的
5/6/7/8/9策略不变，reader 支持1–10。新源码 Resolver 在自己的新池中安装
Iterator.Item 与 IntoIterator.Iter 的 Required 声明及受检签名；通用
TypePool::with_intrinsics 与 archive restore 不注入这些声明。旧 schema 的
has_next/next=None、空关联列表及旧 builtin ABI 按保存的定义执行，已通过
真实独立进程执行并核对读入、字节一致重存与元数据不迁移。

新的 for 计划仅供编译期使用，归档保存已冻结的直接函数调用、具体结果 Enum
和模式指令；无运行时方法名重选，也不引入 continuation/GC 布局变化。

### 稳定类型身份来源（TPOL11）

已finalize的池始终写11，前缀与TPOL10的描述符、索引和元数据表相同，
尾部增加必需的TypeIdentityInput：schema、包上下文列表、名义声明列表。
声明保存局部type_index/package引用、带标签路径和最后稳定版本。尾部整数字段
沿用小端；类型hash的规范字节使用大端，两者不得混淆。

完整公开字节协议见[稳定TypeId协议](../stable-type-id-schema.md)。writer拒绝
丢弃finalized来源的降版。reader支持旧1–10，保留其原索引/128位值/ZERO，
不按新bootstrap角色编号改写旧ID，不宣称有稳定用户类型来源。没有finalize
的池仍按已有5/6/7/8/9/10选择规则写入。NSAM6、外层NSBC3保持不变。

恢复时从受检实际描述符和保存输入重计算全部稳定ID，检查完整两个word、
逆向canonical映射和角色认证。别名共享目标ID但不成为新的原生角色所有者；
伪造布局、错误alias目标和独立重复角色仍拒绝。抽象名义metadata允许ZERO
用于尚未专化的声明，NSBC可执行签名、全局、常量和类型指令继续拒绝symbolic
类型，不以“允许metadata恢复”放宽执行边界。

输入字符串/数量、路径深度和编码/hash工作受预算限制；递归名义图采用有限
anchor边，无递归hash调用。包身份只是本归档声明的输入；此检查不携带外部
清单证明，不认证manifest或完成跨包链接。最终跨进程/篡改/质量门结果见
[实施记录](../stable-type-id-audit.md)。
