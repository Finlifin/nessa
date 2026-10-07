# 包身份与依赖图实施记录

完整 Nessa 目标继续未完成。本专项实现稳定 TypeId/Error 所需的真实包身份
输入：清单解析、约束 ADT、确定依赖 DAG、Merkle 128 位身份及 package.lock。
它不等于完成源码 TypeId 分配、Error 执行或跨包链接。当前生产和独立测试
均在隔离副本实施，尚无通过声明。

## 实际依据与缺口

上轮双变参已独立验收，root/grader workspace1579/0/1。下一阶段实际 CLI
`enum E{bad};fn main(){let value=error E.bad;println(type_of(value))}` 输出 Unit；
Error 构造甚至省略操作数副作用，传播/消除也落到 NIR Unit。基线源/日志
`/tmp/nessa-error-flow-baseline.{ns,log}`，独立更完整探针
`/tmp/nessa-error-flow-repros/{results,corrected-and-archive-results}.json`。

只读 Error/身份调查先于图设计，完整报告
`/tmp/nessa-error-flow-investigation.md`、
`/tmp/nessa-type-identity-investigation.md`。当前 TypePool 有128位 TypeId，
但源码类型多为 ZERO，不能用作非零错误标签。包管理器只有内存清单对象，
没有实际 TOML 解析/hash/lock；选依赖按注册顺序，visited 按包名吞冲突/循环。
清单只保留五个字段，直接据此 hash 会遗漏元数据与未知字段。

详细规范 [包管理](../code-orgnization/package-management.md) 要求清单内容
排除自身版本与全部依赖约束的 Merkle128 身份；
[TypeId](../type-system/type-id.md) 另要求包身份、最后稳定版本、layout和完整路径。
最后稳定版本/临时源码身份以及 Error 消除未匹配语义已由用户明确选择，
后续实现按这些规则推进；本包清单/DAG 专项原验收范围不变。

## 已冻结的契约

解析保留每个语义清单字段，包括未知元数据；校验必填 domain/name/version/type。
只有 package.version 从 PackageId 排除，其它路径上的 version 字段仍参与。
TOML 按类型编码、table 键排序、array 保持顺序；依赖约束按语义 ADT 编码，
包含限定包名与递归选中子包身份。具体选中版本由 lock 固定，不冒充 PackageId。
哈希算法此前规范待定，本专项选用版本1域分离的 SHA256 前128位，大端两个word；
确切字节编码与lock schema需记录可重算的公开产物，不能只断言“hash不相同”。

依赖解析按全部同时约束选择最高兼容版本，必要时回溯；处理零主版本 caret、
精确/tilde/range/prerelease。缺依赖、冲突、循环、同名同版本不同内容均明确拒绝。
版本化 lock 必须保留精确版本/身份，复用时不能悄悄选择更高版本，损坏、过时、
缺失、重复及多余项均拒绝。现有 Version/PackageManifest 五字段与resolver API
保留；完整清单元数据由 ManifestDocument 持有，避免要求旧struct literal增字段。

## 并发与验收

使用 graph-engineering-workflow。两个只读调查完成后冻结 C1-C12，再分派两个
隔离 writer，root唯一合并；Cargo只有一个所有者。graphify无命令/既有图，rg回退。
基线 `/tmp/nessa-package-identity-baseline.tar`；图
`/tmp/nessa-package-identity-graph.json`。上限6角色、4并发、6波次、2重试、1层、
5400秒、12个worker turn、3轮grader；时间采用前一阶段最终clock的保守起点，
包含全部调查/设计时间，未将freeze当作新的计时开始。

独立测试从冻结规范与公开 API 写入，执行前只刷新最终生产源码；grader仅读取
最终产物、基线、scope、实际日志与运行记录。C11验收清单/身份，C12验收DAG/lock
和质量。C4外部研究不适用，C1/C2/C7/C8运行记录核对。未运行完整baseline-red套件。
全部实现、专项测试、workspace fmt/check/test/strictClippy/diff与独立验收均待实测。

用户已明确后续语义：Error 消除隐式成功分支保留值，未处理错误保留为剩余
限定值；只有后缀传播 `!` 提前退出，`ok!` 中 ok 是普通成功载荷绑定。类型的
最后稳定版本可显式提供，缺省用包版本；临时源码由规范化内容生成独立包身份。
这些决策已解决，包身份专项原验收范围未改变。

## 编码复核与局部验证

确切版本1字节/lock/API契约记录于[协议](package-identity-schema.md)。两份
独立Python编码分别为 `/tmp/nessa-package-identity-root-golden.json` 和
`/tmp/nessa-package-identity-tests-independent-golden.json`；简单leaf清单
全部128位期望为 `e902866d7a93e474b9f634b76df12ed5`。独立测试23组已经
准备，但只有生产树的不可变验证副本实际通过23/23，作者尚未取得Cargo独立执行。

Root在执行前发现两处需修正的公开编码细节：inline依赖表未知属性必须保留；
省略版本分量在另一个比较器显式准许预发布时可能改变约束语义。后者修正为
匹配始终使用原始semver请求，只在不含预发布比较器的合取中规范化等价的
稳定版本边界。公开编码与实际源码已对齐，空依赖golden未变；未改冻结C11/C12
要求，也未为哈希等价而改变版本匹配。

生产局部13/13及不可变独立测试副本23/23通过；最初strictClippy指出生产
不必要lazy closure，作者修复；后来指出测试两处可用from_ref，交测试作者
修复。原失败日志保留，没有隐藏或抑制lint。仍待独立作者最终执行及全
workspace/独立grader。源码大清单通过Arc在搜索分支间共享，避免每一步
深复制完整TOML；搜索与DAG/hash遍历使用显式工作栈，资源超限明确报错。

独立作者最终执行24/24通过，新增预发布精度用例覆盖 partial `<`、`>=`、
caret 与 tilde；未准许预发布的稳定边界等价仍成立。513包的正向链与4097包
的隔离子进程均实际解析并重放锁文件，没有依靠预算错误作为深图通过。
目标strictClippy与fmt均通过，最终自有测试补丁与10个生产路径逐字节核对。
根目录先启动的工作区检查早于测试最后合入，日志单独保留；最终全质量
检查在合入后重新执行，不以前一次缺少独立测试的结果冒充最终验证。

Root最终workspace 1610/0/1，fmt、all-targets check、strictClippy和diff均退出0。
独立grader待验收，不能以root质量门代替独立判定；完整项目目标仍未完成。

独立grader复跑workspace1610/0/1、全部质量门、专项24/24及128位独立重算
一致通过。初版10/11适用项通过，C10指出历史TODO仍称包hash算法待定；
root仅修该文档，初版verdict保留，grader对最新17路径hash/patch和全
C1-C12再核对，第二轮11/11适用项通过（C4不适用，C1/C2/C7/C8记录核对）。
修复没有改生产、测试、验收标准或预算。最后只追加此验收记录并核对差异。
包身份专项完成；源码TypeId/provenance归档验证、driver包接入与完整Error
执行仍待完成，项目总目标继续有效。
