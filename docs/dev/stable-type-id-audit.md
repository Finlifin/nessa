# 稳定源码 TypeId 与归档身份输入实施记录

本轮实现已合并，并已通过全工作区复核和独立验收。下文保留调查基线
与首败修复记录；不能以已有包身份或128位存储替代这些验收。
上一轮包身份基础已通过独立验收；本轮检查现有注册、源码上下文和归档后，
冻结完整源码身份计算、原子发布、TPOL11 输入持久化及重计算验收标准。

## 实际基线与缺口

源码名义/结构类型多数为 ZERO；bootstrap trait 的低word来自 StrId。
具体 IterationStep 校验要求 ZERO，抽象模板的 ZERO则是必要设计。
TypePool lookup 把 ZERO 映射到最新注册项，现有归档验证只查描述符一致性，
不能拒绝唯一但伪造的128位ID。旧TPOL1..10已完整保存两个u64，但没有来源输入。

调查报告 `/tmp/nessa-stable-type-id-current-map.md` 与
`/tmp/nessa-stable-type-id-archive-test-map.md` 在分派实现前核实当前源码。
后来发现 Parser 并未填写 Ast.source，因此不能凭该字段存在就推断真实输入
已经保存；源码所有者正在显式保存或从 SourceMap 取输入，位置只用于定位，
不进入身份字节。没有真实源码/显式上下文的手工AST必须给诊断。

## 冻结范围

完整契约 `/tmp/nessa-stable-type-id-contract.md`。类型身份采用版本1域分离
SHA256前128位，完整路径、有效稳定版本与实际布局进入有限可达名义图，
递归边引用稳定anchor。引用布局改变传播，无关声明与注册顺序不影响ID。
alias等于目标，结构类型保留有序槽位；限定成员按语义集合编码。
Trait参数视图可执行，不能因含Trait就一律标为抽象；真正未专化的binder/
template及复合类型仍ZERO。保留原生角色，bootstrap trait改为版本化ordinal。

源码上下文支持包图身份、缺省包版本、显式全局/逐路径最后稳定版本。
临时源码按保留布局和literal内容的token流规范化，排除位置/计数/StrId。
规范化摘要进入真实临时清单metadata，再使用现有包身份协议；std单独真实
清单上下文，不随用户源码/覆盖版本变化。

TPOL11携带包/路径/版本输入并从描述符重计算，拒绝重新计算checksum后的
高低word、输入和layout不一致。旧1..10保留原索引/ID/ZERO和执行语义，
不补造身份，不提升为稳定用户错误标签。此验证只证明与给定包身份一致，
不认证外部manifest，也不构成跨包linker。完整Error、新type源码语义、泛型
和多文件包编排仍是后续项目任务。

## 隔离与验收

root唯一合并，pool+codec、source上下文、独立测试三写者在独立422文件
基线副本中并行；测试从冻结契约与公开schema编写，最终源码刷新后执行。
独立grader只读取最终产物、真实日志、rubric和运行记录，复跑全部质量门。
图 `/tmp/nessa-stable-type-id-graph.json`；7角色、4并发、10波次、3重试、
1层、10800秒、18个可观测worker turn、3轮grader。起点包括调查和设计。
graphify不可用，以rg回退。C4无外部研究不适用，运行记录项明确标注核对。

公开schema `/tmp/nessa-stable-type-id-api.md`、源码API
`/tmp/nessa-stable-type-id-source-api.md`。root独立Python按协议编码P的单字段
布局golden `/tmp/nessa-stable-type-id-root-golden.json`，预期完整128位
`67e86291f31bf3c9777c2a6f4508fed2`。隔离池专项测试已实际断言并通过该值；
源码和独立跨进程测试仍待最终接入验证。

## 构建空间恢复

实现期间sandbox挂载因配额耗尽而在命令启动前失败，不能当作代码编译失败。
沙箱外df请求被用户中止；用户随后明确要求cargo clean及今后自主清理。
root在默认sandbox实际clean成功，删除28672个可生成文件、20.8GiB。

根工作区新增低debug/noincremental profile和`bash scripts/cargo.sh`入口。
入口持有目标目录外的flock，默认缓存达到6GiB就先自动clean；同目录所有
代理均使用该入口。授权写入AGENTS.md，不修改宿主权限policy，不删除源码/
日志/验收产物。root将这些4个自身所有的配置/入口文件同步至隔离树，
属于用户追加任务范围，不混入生产writer所有权或更改C11/C12。

入口syntax/version/metadata与diff检查通过；独立临时stub验证自动clean先于
构建以及并发check/test不交错，记录 `/tmp/nessa-cargo-clean-guard-smoke.json`。
增量profile和调试信息可临时环境覆盖用于Rust调试；不以wrapper取代单一
Cargo所有者。源实现、专项测试和最终工作区验收继续进行。

## 验证与修复过程（保留阶段性记录）

隔离池/编解码初次专项测试为78/81个通过，相关all-targets check和严格
Clippy通过。源码最终依赖刷新后，driver测试11通过3失败、resolution测试
136通过8失败：透明std List/Map别名获得目标reserved ID后，集合校验将
别名误当作第二个角色所有者。首败日志保留，缺陷按所有权回送池实现者。
修复要求别名canonical目标ID一致且通过布局认证，角色唯一性只统计实体
描述符；不得放宽伪造/错误目标/独立重复描述符拒绝。另一个源码作者测试
缺少触发缩进token的冒号，已按真实词法规则修正fixture，不改变规范化算法。

池已有abstract Holder的ZERO/restore测试，现有NSBC拒绝symbolic执行测试
也通过，但这不能替代finalized abstract nominal Holder的可执行拒绝回归。
该缺口明确交给独立端到端测试补证，最终workspace及grader尚未执行。

集合别名窄修后，池79/codec81专项、all-targets编译、严格Clippy通过；
源码driver14/resolution145及推断子进程1通过，既有nessa-test集成663通过。
root读回补丁及hash并已合并pool12/source11文件，整份422文件基线扫描
无丢失和未授权变化。源码newtype仍给明确未实现诊断，不能冒充完成。

独立行为初次13通过7失败，来自fixture假定别名名唯一、遗漏路径末尾声明名
和把importalias误当复制描述符；修正为canonical目标集合、逐别名完整ID
和真实声明路径后20通过。原失败日志保留。归档fixture导入公共API位置、
Required关联声明及writer错误分类修正后7通过，包含删源/污染interner进程
执行和finalized Holder元数据往返。Holder的五种可执行使用均被writer精确
拒绝：global、Type常量、函数返回签名、NewObject、TypeCheck。Reader的
重算checksum负例与全std声明身份证据继续补证，未完成全工作区/独立grader。

全workspace首跑在NIR23项中1通过22失败。实际回读证实主要是手工AST
fixture不保存对应源码，而新身份诊断在空SourceMap上触发原诊断渲染器panic；
最初把所有调用归为bare Parser的推断已纠正。窄修由同一源码所有者在隔离树
完成：Parser统一保存真实输入、手工AST助手保存对应源码，缺来源的AST
仍给错误，两个诊断消费者通过受检source/span共用plain回退，不生成假的
SourceFile、不吞错误或关闭identity校验。针对空map和不可定位位置保留
计数、message、labels、notes与helps的回归已通过；最终合并与workspace复跑待定。


## 最终合并后的实际检查

源码窄修已合并。`Ast.source` 使用 Option 区分已知空源码与未知来源；Parser
在统一完成路径保存真实输入，手工 NIR fixture 保存对应源码，两个诊断消费者
共享受检 span/source 定位及纯文本回退。缺失来源仍是错误，不伪造身份。

独立测试最终20个行为组和7个归档组通过。finalized Holder 覆盖五种 writer
拒绝、有效 checksum 下的 reader 拒绝及新进程 CLI 不执行；全 std 原始名义
声明身份也逐项验证。早先未完成这些补证的段落是过程记录，不是当前缺口。

根工作区最终 fmt、all-targets check、全量 test、严格 Clippy 均以退出码0结束。
全量测试1668通过、0失败、1个既有忽略；实际日志为
`/tmp/nessa-stable-type-id-root-{fmt,check,test,clippy}-final.log`。
独立验收尚未执行，不能把根检查当作独立验收或整个 Nessa 目标完成。

清理任务最后实际移除3948个可生成文件、2.6GiB；入口自动阈值清理与
并发串行 smoke 通过，后续构建无需重复请求清理授权。

类型身份的 schema/算法字节与 source context 公共字段未改变。早期独立测试
读取的 source API 文件哈希保存在
`/tmp/nessa-stable-type-id-source-api-before-repair.md`，后续原文件只追加了
来源 Option 契约说明；最终内容范围另以 root final manifest 核对。

本阶段不声明实现完整 Error 运行时、源码 newtype、完整泛型、多文件链接器
或外部包 manifest 认证。Continuation 多栈 ABI 和其余设计目标仍须按总目标
持续验收。


## 独立验收结论

全新上下文 grader 复跑 fmt/check/test/严格 Clippy/diff 均退出0，实际测试
1668通过、0失败、1个既有忽略。独立重建三份 golden 的规范字节与完整
128位摘要相符；TPOL11、旧版本、删源执行及篡改/抽象执行拒绝回归通过。
验收结果 `/tmp/nessa-stable-type-id-grader-result.json` 为11/11适用项通过，
C4无外部调查不适用，C1/C2/C7/C8属于运行记录核对，不能称独立行为证据。

验收还发现 root 导出的整体补丁未处理无末尾换行的旧 .gitignore。已保留
原损坏补丁并补齐 Git 标记；grader 在422文件原基线独立应用补丁，44条
最终文件哈希逐项吻合。该修复只影响交付产物，没有修改实现或验收标准。
原验收范围/hash保存在 `/tmp/nessa-stable-type-id-graded-paths.json`；随后
仅更新本实施记录、协议状态和进度说明，生产实现及测试未变化。

稳定身份及来源持久化专项完成。完整 Nessa 目标仍未完成，完整 Error 的
静态/运行时/GC/持久化路径正在调查，原生 continuation 切换等仍需完成。
