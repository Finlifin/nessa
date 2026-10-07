# 单步带标签迭代协议

用户已选择新源码采用单次 `next` 和带标签迭代结果，替代旧的
`has_next()->bool` / `next()->Item` 调用对。本文件记录要求与调查证据；协议
静态协议现已接通。结果的源码拼写为 `IterationStep(Item)`；builtin Iterator
要求显式 Item 绑定，IntoIterator 要求显式 Iter 绑定。动态关联 carrier 仍未接通。

## 必须保持的语义

- 每轮只消费一次 `next`，结果明确区分结束与一个元素。`null`、Unit、空 Tuple
  均可作为元素，不以其值或 truthiness 判断结束。
- 结果载荷对应所选 Iterator 的精确关联 Item。不能把公共返回契约永久降为
  Any，或仅依赖 `for` 内的一次 cast 冒充完整类型支持。类型反射、直接 next
  调用、构造、模式匹配和实际返回检查必须一致。
- IntoIterator 的具体迭代器类型及其 Iterator 义务按精确 impl/scope 检查；
  关联绑定和实际 next 目标不能在 lowering 或运行时按名字重新猜测。
- 输入及 into_iter 只求值一次。空迭代器、单元素、多元素、continue、break、
  嵌套/标签循环和有副作用的 next 均需验证消费次数和返回值。
- List 必须支持 null 元素和独立迭代器，保持 GC 根、归档与 continuation
  多次恢复正确。迭代期间列表修改的规则必须明确，不能由偶然越界决定。
- 旧 NSBC 继续按保存的 schema 和指令执行；reader 不给旧 next=None slot
  补造新签名、关联绑定或新结束语义。新源码的旧协议迁移需明确诊断与说明。

## 当前证据和缺口

`trait-definition.md` 的 Iterator 示例仍为 `next()->?Item`，Optional 使用
immediate null，无法区分 List 中的合法 null 与迭代结束。该示例是旧设计，
需要随带标签协议的真实实现一起迁移，不能据此禁止 null 元素。

bootstrap 只登记 `next(self)->IterationStep(Item)` 和 `into_iter(self)->Iter`。
Item/Iter 使用真实 Required 关联声明，未提供绑定的实现显式拒绝。
IntoIterator.Iter 必须在声明作用域实现 Iterator，即使实现没有被循环使用。
调用者的 `for` 在自己的有效作用域选择具体 Iterator，保存其准确 Item 和
源函数目标；具体返回值不携带跨作用域的隐藏 Iterator 证明。

`ForLoopPlan` 在 body 检查前确定 Item、结果类型和精确 impl/scope，完成
trait 检查后冻结函数 ID。默认方法按各实现保存、恢复独立计划；NIR 直接调用
计划中的函数，不重新按方法名选择。隐式 next/into_iter 也进入模块初始化
依赖分析。输入、into_iter 各执行一次，每次回到 header 只调用一次 next。

支持的可失败模式不匹配时跳过元素；tuple、enum 和 guard 使用统一模式
分支。未支持的模式继续明确诊断，不声称所有语法模式都已实现。
ListIterator 持有浅元素快照，独立 into_iter 调用有独立游标；原列表后续
增删改不改变快照，元素对象引用仍共享。null/Unit 均是合法 yielded 元素。

具体结果采用 Enum；依赖 Item/Self 的声明采用专用编译期模板，不赋予普通
nominal Enum 泛型替换能力。结果模板和构造路径已接通，bootstrap 签名与
循环尚待迁移，不能只新增一个 Any 载荷 enum 即声称完成。

## 已完成的具体类型基础

`TypePool::intern_iteration_step(Item)` 创建显式结构来源的 Enum：
`done`（tag 0，无载荷）、`yielded`（tag 1，单个准确 Item 的 value 字段）。
Item alias 规范化后复用实例，不同 Item 和普通同名 enum 保持不同身份。
反射名称为 `IterationStep(Item)`，恢复后仍保留身份；VM 沿用 Enum 的实际
载荷检查与布局，null 元素不表示结束。符号关联类型和 trait carrier 在此
工厂中显式拒绝，必须先完成专化。

仅有具体结构来源的池使用 TPOL8；依赖模板使用 TPOL9；Required 关联声明
使用 TPOL10。新源码包含 builtin Required 声明而写10，其他池仍保留5–9策略。旧 TPOL1–7
拒绝 Enum 的结构来源标记，不按同名描述符补造结果身份。

具体源码工厂现已接入：std builtin 导出编译期工厂绑定，导入/重命名/
工厂别名保持绑定身份，普通同名声明不获得工厂能力。实例可用于类型注解、
类型值、done/yielded 构造与载荷模式匹配。错误载荷和非法类型参数被拒绝。
删除源码后独立运行归档、真实 GC 和多次 continuation 恢复已覆盖具体结果。

Item/Self 模板路径已贯穿声明签名、关联默认表达式、默认方法体与关联别名。
嵌套 Function/Tuple/Optional 保留准确载荷，默认方法中的构造、匹配与类型值
按精确 impl/scope 重新检查。编译期模板不具有对象布局；可执行签名、常量、
全局与类型操作拒绝未专化模板，包括不含关联 marker 的 Self 模板。
TPOL9 检查模板来源、路径、默认表达式和降版；普通同名 Enum 不获得替换能力。

Iterator/IntoIterator 静态签名、单次 next 的 for 降低和 List 接入已完成。
公共动态关联 trait carrier、泛型 List 和更多模式仍是后续工作。

```nessa
typealias Step = IterationStep(i64)
fn next() -> Step { Step.yielded(42) }
fn main() {
  next() match { Step.done => 0, Step.yielded(value) => value }
}
```

## 实施顺序与验收

先落实结果载荷类型及其 Item 路径的 producer/consumer，包括类型池、解析与
类型检查、构造/匹配、VM、GC、归档及损坏输入校验。然后登记新 bootstrap
Iterator/IntoIterator 契约，生成一次确定的 for 协议计划，再降低循环和接入
List。按各层真实写入边界拆分工作，旧产物另做不迁移语义的执行回归。

验收覆盖直接 next 的正确/错误载荷、精确 scoped 关联绑定、null 元素、空结束、
准确反射、模式载荷、消费次数、源码删除后的独立归档、真实 GC 与多次恢复。
结果类型有符号声明时，所有可执行签名和对象必须已经具体化。公共动态 trait
返回/存储 carrier 仍是另一个待完成协议，不借此隐式开放。
