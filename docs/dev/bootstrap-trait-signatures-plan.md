# Bootstrap trait 接口签名补全计划

本文件保留 TPOL5 阶段的调查依据，并随实现更新。新 source producer 已为
Eq、PartialEq、Display 和 Iterator.has_next 提供固定签名与真实 Self 路径；
其他 bootstrap 槽仍为 None，不根据某个实现猜测关联返回协议。裸 trait 参数
proof 已接通；trait 返回逃逸和关联类型协议仍未完成。

## 当前数据来源与缺口

`library/std/builtin.ns` 为 Display、Hash、Eq、Ord、PartialEq、PartialOrd、Iterator、
IntoIterator 提供 `'builtin` 类型别名，没有语言级方法声明。
`engine/type_pool/src/lib.rs::register_well_known_traits` 注册八个空的 Trait 描述符：
没有父 trait、关联类型或接口函数签名。`engine/resolution/src/resolver.rs` 把这些类型
发布给 builtin 类型目录；根作用域符号使用 `NodeIndex::NULL`，没有可供分析的 AST。

`engine/resolution/src/trait_schemas.rs::bootstrap_names` 是当前固定 slot 的来源，
仅按 trait 身份指定名字。调查时 `schema` 的 bootstrap 分支全部写入 None；现在 `bootstrap_schema`
在可变 register 阶段 intern 上述固定 Function 声明，其余槽保留 None。源语言
trait 的分支由 `trait_signatures::signature` 从 AST
提取声明类型、Self 路径与参数类别。二者不能混为“已有完整接口”。

TPOL5 能保存完整签名，但不能凭空补足上述来源。当前 NSBC validator 的
`methods` 检查仍跳过 None 的普通接口匹配。具有 Some 签名的
DERIVE_FUNC_ID 目标现在通过 type_pool::native_derived 的共享可信契约检查，
不把 sentinel 当作普通函数；raw method table、trait impl、vtable 与 VM
实际选中的 slot 均检查归属。其余 table 身份、slot 顺序、函数存在性与
capture/arity 检查继续执行。

## 已有行为支持的契约

以下签名包含 receiver，属于实际函数 ABI，不是移除 receiver 后的绑定方法类型。
固定 Self 路径可由受信任 bootstrap 定义直接提供，无需扫描不存在的 AST。

| Trait / slot | 可依据当前实现确定的契约 | Self 路径 / 参数类别 | 实现与边界 |
| --- | --- | --- | --- |
| Eq.eq | `fn(Self, Self) -> bool` | Parameter(0)、Parameter(1)；Receiver、Required | 用户实现已有独立检查；aggregate 派生已生成普通函数 |
| PartialEq.eq | `fn(Self, Self) -> bool` | 同 Eq | 同 Eq，不能因名字相同混淆两种 trait 身份 |
| Display.to_string | `fn(Self) -> String` | Parameter(0)；Receiver | struct native 派生实际分配并返回 String；用户实现现已按固定声明统一检查 |
| Iterator.has_next | `fn(Self) -> bool` | Parameter(0)；Receiver | `trait_loops::method_return` 已要求单 receiver 和 bool，但仅在 for 消费路径检查 |
| Ord.cmp | 当前 native ABI 为 `fn(Self, Self) -> i64` | Parameter(0)、Parameter(1)；Receiver、Required | runtime 返回 -1/0/1；与设计中的 Ordering 结果冲突，不能静默声称规范已落实 |

Eq/PartialEq 的依据是 `engine/resolution/src/comparison_derivation.rs`：
用户 `eq` 必须有两个参数，二者 canonical 类型都为 implementor，返回 bool；
派生计划保存的真实函数签名也是 `[implementor, implementor] -> bool`。
`declared_method_signature` 暴露给绑定方法调用的单参数类型不能直接作为 vtable
目标的实际函数签名使用。

Display 与 Ord 的依据是 `engine/interpreter/src/lib.rs::dispatch_derived_method`
及 `engine/interpreter/src/derived.rs`。native 分支检查 eq/cmp 一个显式参数、
to_string 零个显式参数；struct payload 的类型与槽数在读取前核对。
Ord 当前使用 i64 返回值是实现事实；[trait-definition.md](../type-system/trait-implementation-extension/trait-definition.md) 则要求 `Ord(Eq)`、Ordering 结果及默认关系操作。这些父接口、
结果类型和默认方法目前都不能靠补一个签名宣称完成。

## 尚不能以固定签名替代的接口

- `Hash.hash`：源码 Hash 派生明确拒绝，runtime sentinel 分支返回
  `UnsupportedDerivedMethod("hash")`。现有受支持路径不足以确定其返回类型，不能
  选择 u64/usize/i64 并把该选择当作已存在的契约。普通用户 hash 方法虽有各自
  Function 类型，不能由某一个实现反推整个接口。
- `PartialOrd.partial_cmp`：bootstrap slot 名为 partial_cmp，但派生被拒绝，
  runtime 没有相应 native 分支。现有扩展测试使用 i64 返回值，不足以证明统一的
  部分序结果协议。`generate_derive_methods` 的 cmp 历史分支也不是可执行的
  PartialOrd 接口依据。
- `Iterator.next`：当前 for 路径接受实现自己的返回类型，已有 next 返回 i64
  的有效回归。设计中的 `?Item` 需要关联类型绑定；bootstrap 描述符没有 Item。
  当前签名检查采用不变匹配，填 `Any` 会拒绝这些具体返回类型，不能作为兼容修复。
- `IntoIterator.into_iter`：当前实现可以返回具体 Iter，随后按该类型寻找 Iterator。
  填固定 `Iterator` 返回值同样不满足现有不变匹配，也没有保存关联 Iterator 类型。
  需要明确关联类型或其它完整的接口实例化机制，再补可信签名。

可信的 `has_next` 可先独立补全，`next` 保持未完成；但含未完成 slot 的整个 Iterator
接口仍不能被宣称具备完整动态 proof 契约。List/Map 的当前 std 方法没有因此自动
获得 Iterator/IntoIterator 实现。

## Native 目标必须单独核对

`DERIVE_FUNC_ID` 是共享 sentinel，不是拥有 `Function.function_type` 的普通函数。
同一个 ID 当前按方法名进入 eq、cmp、to_string 或显式未支持的 hash 分支。
它不属于 `CallBuiltin` 原生函数 ID 目录，也没有相应 builtin import manifest。
全局 `to_string` builtin 是独立转换函数，不能与 Display 的 sentinel 目标混用。

新增接口签名后，需要同时增加受信任 native 契约验证：

1. 按 canonical trait 身份、slot 名、receiver 类型/layout 共同识别支持的 native
   目标，不能仅按名字或 sentinel ID 授权。
2. 明确验证其实际 receiver/参数数量和结果类型。Eq/PartialEq 的新编译产物优先
   使用已有 generated 普通函数；旧 struct sentinel 的运行时 layout 检查仍需保留。
3. Display 与 Ord native 仅支持已有 struct 路径。不能把 tuple、enum 或任意自定义
   trait 的同名 slot 自动视为该 native 实现。Hash/PartialOrd 未支持目标明确拒绝。
4. 检查用户普通目标时，继续使用 key 的签名及 Self 路径，与真实
   `CompiledFunction.function_type` 匹配，并执行现有 capture/arity 守门。
5. 接口声明不证明 bytecode 函数体一定返回声明类型。未来 proof 分派还应考虑返回
   边界检查，不能把 metadata 匹配扩大为任意损坏函数体都已类型安全的承诺。

## 可复现缺口

下面源码在调查时能编译并输出 `42`；现在两个错误接口签名均在 resolution
拒绝，即使 main 不调用它们：

```nessa
struct Value {}
impl Display for Value {
    pub fn to_string(self, value: i64) -> bool { true }
}
impl Iterator for Value {
    pub fn has_next(self) -> i64 { 42 }
    pub fn next(self) -> i64 { 42 }
}
fn main() { println(42) }
```

调查复现文件为 `/tmp/nessa-bootstrap-signature-bad.ns`。has_next 的错误在没有 for
消费时曾漏检；Display 的额外参数与错误返回值亦曾缺少检查。新增回归
直接检查无调用程序的诊断；合法 next 返回具体 i64 的旧路径保持。

Eq/PartialEq 不能作为“目前所有 bootstrap 都接受坏实现”的证据：
`impl Eq for Value { fn eq(self, other: bool) -> i64 { 42 } }` 已被现有独立比较检查
以 `Eq/PartialEq implementations require eq(self, other: Self) -> bool` 拒绝。
新 source producer 已独立生成 TPOL5 固定声明；旧 archive 的 None 不由该
检查补造。Eq/PartialEq 参数比较与直接接口方法均消费同一固定签名。

## 建议实施顺序与兼容边界

先为新 source 的 Eq、PartialEq、Display、Iterator.has_next 建立固定声明及 Self
路径，随后接入完整的普通/native 目标检查。Ord 先明确当前 i64 ABI 与 Ordering
设计的差异；Hash、PartialOrd、next、into_iter 单独完成协议和关联类型后再补。

声明所需的结构 Function 类型应在已有 intrinsic/trait/null prefix 建立之后追加，
例如由 resolution 的 bootstrap schema 构建阶段 intern。不要在
`register_well_known_traits` 中插入 Function 类型而移动既有 intrinsic/null 索引。
当前 `schema` 使用不可变 Resolver，可先在 `register` 的可变阶段建立固定声明，
再让 schema producer 读取对应 key。固定契约应只有一个权威定义，避免 resolver、
loader、VM 复制三套不一致的名字表。

新 source producer 可以明确提供新的可信签名。旧 TPOL1–4 没有签名，旧 TPOL5
也可能合法保存 `None`；reader 和重写 writer 都不能依据名字、well-known trait
索引或某个实现函数猜测补齐。保持 legacy ordinary metadata 的原解释，但没有
完整签名的接口不能获得新的 proof 授权。该阶段不要求修改 NSAM3 或 bytecode ABI；
native 契约若需要额外可持久化身份，应另行明确格式和旧版拒绝策略。

## 完成回归要求

- 新 source 的上述固定 key 必须有精确 declaration、Self 路径和参数类别；
  std builtin alias、implementor alias 和 scoped extension 保持 canonical 身份。
- 坏 receiver、参数类型、返回类型、arity、optional/variadic 模式在没有调用该方法
  时也应诊断，且不得输出归档。
- Eq/PartialEq 的真实 generated 函数、Display native 与明确后的 Ord native
  分别通过源码和删除源码后的跨进程归档执行；字段类型及 GC 分配路径保持正确。
- 归档往返后固定签名与 inherited schema 完全一致；替换 target signature、交换
  trait 身份、伪造 sentinel 目标、损坏 receiver layout 都必须明确拒绝。
- 旧版与 `signature: None` 的保存/升级不能产生新签名；其不能参与新 proof 授权，
  ordinary legacy 读取行为及已有显式未支持错误保持可审查。
- Iterator/IntoIterator 现有具体返回类型的有效源码不因把返回类型伪装为 Any/Iterator
  而被拒绝；关联类型协议完成时补完整正例、拒绝例和持久化验证。
- 最后运行受影响 crate、nessa-test、CLI source-delete tests 及 workspace 质量检查；
  分别报告接口签名、native 验证和 proof 执行的实际完成范围。


## 固定接口的静态方法消费

methods.rs 的 bootstrap 方法投影复用 bootstrap_schema 固定 key，并将真实
Self 路径特化为 receiver view。继承接口的 Self 参数仍是 child view；查到
源码声明时优先检查该声明及访问权限，不因权限错误退回父 builtin 接口。
Eq/PartialEq 的 eq 与 Display.to_string 可经裸 trait 参数真实调用；绑定为
方法值仍明确未支持，具名参数不因缺 AST 而猜造参数绑定。

for 的裸 Iterator/IntoIterator 输入和 into_iter 返回 trait 的路径，在关联
返回 proof 契约完成前于 resolution 明确诊断；现有 concrete Iterator 的
has_next/next 调用与具体 next 返回类型保留。这是缺口守门，不是完整迭代验收。


## 原生派生守门的实际范围

check_native_derived_method/slot 只授权新 Some 契约下的可信 Eq/PartialEq.eq
和 Display.to_string：同时检查 bootstrap 保留 prefix/TypeId namespace、
canonical trait 与槽归属、精确 Self 路径/参数类别/结果类型及普通 Struct
连续 TaggedValue 布局；reserved collection、非 struct、错误字段布局拒绝。
不比较 TypeId 的第二 word 与进程 StrId，避免破坏归档字符串重定位。

检查返回原生目标身份，VM 保留实际选中的 MethodSlot，不仅保存 FuncId。
自定义同名 Eq、修改 well_known 指向同 namespace 的假 trait、交换接口、
错误 signature 和 layout 均有拒绝回归。旧无 schema/无签名 metadata 维持
原解释，不由名字补造授权。该守门不等于原生 proof adapter：TraitProof 对
sentinel 目标仍 Unsupported，Ord 的 Ordering、Hash 与关联迭代协议仍待实现。


## 新 source 的原生 Display 证明适配

NativeDerivedPlan 为全局 struct Display 派生建立具体 `fn(implementor)->String`
签名；NIR 生成 ordinary Value 参数 wrapper，经 derived_methods publication
替换 method/record/vtable 的 sentinel，继承的槽亦保存真实 FuncId。既有
Eq/PartialEq generated functions 同样可通过完整固定接口证明执行。

wrapper 调用 builtin ABI4 的 ID120 `__derived_display` 并检查 String 返回。
native 复用旧 derived_struct_to_string 字段格式；同时核实可信 Display
contract、struct metadata/layout、当前 ordinary 函数为已发布的全局目标、
具体签名及无 capture 的 Value ABI，不按接收方 scope 重选。native 原始
Any 调用不能绕过该 wrapper 验证，错误 payload、caller 或返回类型均拒绝。

source structs 的字段元数据现在同步 size=count×8 与 align=8，和实际
TaggedValue payload 一致；先前仅更新字段列表会让非空派生类型未满足
受检 native layout。此修复不改变实际字段槽或 GC 扫描。

旧 artifact 的 sentinel、None/LegacyUnknown 与缺 manifest 保存原解释，
不补造 wrapper 或新 proof 授权。新 native manifest 必须声明准确 ID/name；
ABI3不能声称使用120，ABI1/2原限制保持。Ord/Ordering、Hash、intrinsic/
Enum/tuple Display 与关联迭代仍未完成。
