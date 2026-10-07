# Trait 默认方法具体 adapter 实施要求

调查时默认 body 注入其抽象声明 SymbolId，无法作为 concrete vtable 目标。
现在 DefaultMethodPlan 为每个实现产生 fresh SymbolId 与具体 Function 签名，
NIR 再分配独立 FuncId；原声明 body 仍供检查与复用。原全局 function ID
remap 直接发布这些独立目标，不需要按共享声明 ID 覆盖多个实现。

需要按 (implementor, declaring trait, implementation scope, method) 建立独立
DefaultMethodPlan，保存声明 symbol、具体化的 Function 签名和新 FuncId。
同一个 default 可用于多个 concrete 类型及作用域，不能复用一个 SymbolId
映射并让最后一个实现覆盖此前目标。

具体化只替换声明 signature.self_paths 标明的 Self。显式 trait 类型及它的
alias 不等于 Self，不能全量替换相等 TypeIndex。默认 body 内的 Self 类型值、
局部注解、cast 和嵌套闭包也需要源 Self provenance 或受检重新解析，不能仅
修改函数参数类型而留下内部节点的抽象类型。

adapter 的具体 Self 参数使用 TraitSelf ABI，接收原 caller 的 proof、data；
不能在入口按 provider scope 重选，因为 Child 继承的 Base 默认方法可能
依赖已冻结的 Child 覆盖。TraitCall 将原 root table 的证明投影到 adapter
声明 view，并转发额外 Self 参数各自的证明；body 的普通表达式仍用声明 scope。
所需的 Self 类型值从 source paths 专化；显式 trait/alias 值保留原接口身份。

最小真实验收是默认 sum(self)->i64 调用 required value(self)->i64，多个
implementor、作用域的结果分别正确；同名 Left/Right 不混用，child parent
转发保留覆盖，explicit override 优先。identity(self)->Self 的 concrete adapter
可验证具体返回类型；这不代替用户 fn()->Trait 返回/存储语义的决策。

全部目标必须经过 source→NIR→CODE→VM→NSBC，并在删除源码后执行；错误
签名、缺 body 目标、closure 捕获及多实现覆盖需要拒绝或保持正确。GC 与
continuation 仍保存 data/proof 根并拆接独立栈段。当前上述基本 adapter、继承、闭包与多恢复已有源码/删除源码归档/真实 GC
回归。复杂内嵌函数与推断 Self 的完整 provenance 仍需继续验收；不能据此
声明默认方法及用户 trait 返回/存储的所有设计均已完成。


## 内嵌 lambda 的已验证路径

DefaultMethodPlan 的 self_type_values 只专化源 Self 类型表达式，
self_function_types/self_function_parameters 专化 lambda 的显式 Self 注解、
真实 Function 签名与 direct Self 参数位置；这些映射传入 nested builder。
显式 trait 注解及其 alias 保持原接口身份。typed Self 局部副本保留原证明。

纯 Value 签名的间接调用使用逻辑 CallIndirect，由实际目标 FunctionAbi 在
真实 caller scope 适配 TraitSelf；含 bare trait 参数的函数值仍用物理证明
调用保留原 sidecar。这使返回 fn(Self)->i64 的回调捕获原 self 的40，并在
外部调用位置为 other 取得2的独立证明，返回42；不需要新 closure carrier。
零参数显式 ->Self lambda 返回具体 P，nested ||Self 类型值分别为 P/Q。

推断 Self 的完整值流 provenance、默认 body 的内嵌命名函数专化与构造 Self
仍需继续实现/验收。traittyped x.identity() 的返回证明传输没有在本轮决定；
具体 P.identity() 的结果类型 P 已验证，不替代未决 trait 返回/存储协议。


## 推断函数与内嵌命名函数的具体化

新增独立SelfProvenance分析，从source Self参数、推断副本、tuple/projection、
一致if分支及return表达式保存类型路径。变量写入取路径交集；显式Read注解、
cast或混合接口分支截断Self来源。源`fn(Self)->Self`返回或局部注解还为lambda
的省略参数注解提供上下文路径；不能从碰巧等于Read的TypeIndex推断Self。
嵌套深度受检，递归引用保守处理，不绕过签名shape验证。

DefaultMethodPlan同时保存内嵌命名函数的实际签名和direct Self参数。NIR为
每个adapter分配独立的内嵌FuncId，建立局部sourceSymbol→FuncId覆盖表，支持
递归调用和函数值返回；函数收集/生成按AST索引排序，避免HashMap遍历决定ID。
命名函数仍不能捕获外层local，需要capture时使用lambda。Self类型值、lambda
捕获与TraitSelf参数沿用原proof/data ABI，不新增trait返回carrier。

静态调用方在type阶段按source default签名的Self路径具体化返回类型，trait
发布后将普通instance call重新绑定到正确实现和scope的adapter。只改变运行时
Function标签而不改变调用方类型是不够的：归档回归直接调用回调返回值的字段。
当实际return类型因Self专化而变化时，所有return出口检查具体类型；如声明
`fn()->Self`却返回捕获显式Read的闭包，拒绝TypeError，不能谎报`fn()->P`。
显式接口返回类型不通过此机制重选证明或引入返回证明传输语义。

P/Q双实现、精确函数反射、上下文推断参数、递归内嵌函数、显式接口边界、真实
GC及detached continuation多次恢复、删除源码的归档执行都有回归。Self构造
仍需要逐implementor重新检查布局/字段/默认值，不能只替换TypeIndex；复杂
match/loop等值流的完整provenance和trait返回/存储协议仍需继续完成。

## 关联类型默认方法的具体化要求

静态关联默认类型已完成后，默认方法不能只解除原先的 blanket rejection。
每个具体实现需要按准确的 implementation trait 和 scope 读取绑定，专化实际
adapter 的签名、方法体节点、局部类型注解、Type 值、cast 和嵌套函数类型。
显式 Any、显式 trait 注解及 source Self 的来源必须分别保持；Item 绑定碰巧
等于 Self 的具体类型，仍不能把 Item 参数扩成 proof/data 两槽。

先登记所有 impl records 和具体关联绑定，再检查和发布所需默认 adapters。
用户显式覆盖的方法不实例化未使用的默认体。每个实际 adapter 单独类型检查，
一个合法 P 实现不能遮住 Q 实现中的错误；静态错误停止代码及归档生成，Any
边界则保留运行时实际值检查。未实例化的声明保持模板，不发布抽象执行函数。

内部 Self 证明继续携带固定的根实现和方法选择，不能为关联类型改成按 provider
scope 重新选择。含关联声明的内部 proof 必须已有根记录的完整具体绑定；
校验和调用按真实 descriptor 的 implementation trait/scope 专化接口。继承
投影保留原 root table 的覆盖；bare ParameterAbi::Trait 关联视图仍拒绝，这
没有决定用户动态关联 trait 的返回或存储 carrier。

实际证明参数仅来自 source self_paths；关联参数用具体值 ABI。闭包捕获的
Self proof 与 data 继续成对检查并进入 GC 根，continuation 捕获/恢复沿独立
栈段保存，clone 为多次恢复建立独立分支。既有 NSAM5/6、TPOL7 与 builtin
ABI7 能保存具体 adapter、签名、绑定及证明描述，无需伪造新归档信息。

验收包括 Self/Item 同类型而 ABI 不同、两个 impl/scope 的不同 Item、继承的
实际父绑定和子方法覆盖、局部/嵌套函数具体反射、Type 值/cast、显式 Any
保持、静态错误及错误实际返回、真实 GC、多次恢复与源码删除后的归档执行。
本节是该阶段的要求，完成证据以最新 implementation-progress 为准。

实现将具体方法体的重检放在 default_body 模块，收集各 adapter 的 lowering
facts。字段索引、constructor/enum 信息、实参适配、instance/application/update/
concat 调用和 coercion 均使用当前方法体的具体结果；不能回退到另一实现留下的
抽象或具体结果。临时重检结束及出错时恢复 resolver 的原 facts 和符号类型。
长期保存的 facts 只覆盖方法体及其嵌套定义，避免每个 adapter 留存整个模块图。

回归曾复现并修正两类静默错误：P 的 Item.value 位于字段 1、Q 位于字段 0 时
不能共享索引；声明返回 fn()->Item 的默认方法在 Item=String 时不能接受 ||42。
局部注解、命名函数、Tuple 及隐式 lambda 返回也按具体绑定检查。

初始化依赖扫描分别保存当前 body adapter 和冻结根 adapter。Child 默认方法调用
继承的 Base 默认体时，切换 body facts，但保留 Child 的实际覆盖；已选函数
依赖的模块也进入加载图。访问另一个 Self 参数时，当前规划器保守收集同具体
类型的兼容实现目标，没有完整的参数证明数据流，可能增加依赖或形成假循环。
未被写入的简单 receiver 别名链现按原冻结证明扫描，不再收集无关 provider。
receiver 自身或别名被赋值时不作该推断；直接 Self 调用也遵守相同失效规则。
同一模块内的全局初始化仍按源码顺序，前向读取继续报告 UninitializedGlobal。

本阶段关联默认方法的 Self 参数证明仅支持直接参数，包括返回闭包或命名函数
的直接 Self 参数。参数内部的 Tuple、Optional、Function 等嵌套 Self 需要
额外证明 carrier，明确拒绝；Item 的结构参数仍按具体值检查，不据类型相同
推断 Self 证明。Self 返回结构和 Type 值的静态专化不因此禁用。
