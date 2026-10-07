# Enum 默认字段与 List 变参验收

源variant使用独立参数作用域，准确具名类型、`.optional:T=expr`及单个
`...items:List`。显式值先按源码顺序snapshot，然后按声明序填入已选默认值；
默认可引用前置字段，显式覆盖不执行默认，调用者同名变量不改变声明绑定。
变参打包为一个List payload槽，元素可含null/Unit/heap引用。空变参为空List，
不能具名提供，后面只能可选参数。默认闭包捕获前置值，重复构造独立执行。

constructor存入已有CallArgumentPlan，NIR复用函数argument lowering，包括
临时符号绑定与恢复；初始化选中默认、共享startup捕获检查、默认展开depth/
cost/cycle守卫和default adapter facts都消费既有call_arguments。metadata-only
IterationStep构造没有源参数声明，保留明确的固定field fallback，不强造默认。
错误声明在未使用时也检查；无效field数量不进入不安全的metadata索引。

隔离测试作者只新增enum_default_tests.rs，11组真实执行覆盖default-only、
trace982/912、前值snapshot、覆盖、前置依赖/alias/关联helper/caller shadow、
variant作用域、变参/null/Unit、闭包、循环与i8/u128精确边界。root读回后合并
并复跑，另补5组初始化/循环依赖/显式覆盖/default adapter、默认递归与控制
边界、动态Any错误、真实GC和multishot。GC至少4次，结果42且活动栈为0；Enum
在暂停前持有默认闭包、捕获String和变参List中的String/null/Unit。

新CLI归档案例先运行源程序、build、删除源码后另进程执行：具名源码次序与
前置默认、变参/默认闭包、初始化依赖、trait default adapter和显式覆盖。
另验证Any错误归档后仍为TypeError，以及完整field name/type/has_default/
offset和字节一致重存。has_default本来就是字段metadata，默认已降为调用点
普通代码，TPOL/NSAM/NSBC/builtin ABI及旧enum布局没有新增版本或重解释。

graph-engineering-workflow管理依赖、隔离写入和独立验收；graphify缺CLI和现成图
未运行。记录在/tmp/nessa-enum-default-graph.json，日志使用
/tmp/nessa-enum-default-前缀：resolution-dev、baseline、target、archive、full、
fmt、check、clippy、diff。最终全仓1298通过、0失败、1项已有忽略；fmt、全目标
check、严格clippy与diff检查全部通过。独立验收者另行复跑同样检查，结果一致，
日志使用/tmp/nessa-enum-grader-round2-前缀。没有提交、推送、
部署、发布、付款或对外发送。

第一轮独立复核发现普通match的CaseArm被误当独立handler，return可绕过
default控制边界。root新增明确handler arm集合，名称控制检查与共享默认guard
只对实际handler提供独立上下文；普通match沿用外部循环及default边界。回归
覆盖Enum/function/struct默认中的match return、break/continue拒绝与CLI不产出
文件，以及合法match嵌套循环、handler return、lambda return和handler归档。
第一轮12项中C12失败；修复后第二轮重新独立检查全部标准，12/12通过，验收门
通过。原失败记录保留，CLI缺陷探针重新运行确认拒绝且不生成归档。

C1、C2、C7、C8依据工作期间持续追加的记录核对，属于record/attested；其他
标准由独立验收者读取产物或复跑验证。4名参与者（含root），峰值并发2，上限4；
3波、1次重试、深度1、2轮验收，均在预设上限内。保守计入最后的只读记录澄清，
共6个worker turn，达到但未超过6个上限；该澄清未增加功能验收轮次。隔离作者
的原始文件与合并文件一致，原有文件未被其改动，独立验收两次核对隔离边界。

当前完成准确具名参数的可选默认与单List变参，不代表first-class constructor、
解构字段、双变参或泛型Enum完成；公共动态trait carrier与其他项目目标继续推进。
