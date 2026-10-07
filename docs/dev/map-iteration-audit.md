# Map 快照与准确单步迭代验收

Map 的 keys、values、entries 返回独立 List 浅快照。entries 的每个值是准确
(String, Any) tuple，包含 null 和 Unit；原 Map 后续替换、删除、新增不影响
已取得的快照，载荷对象的引用仍共享。MapIterator 保存 pair 快照和独立游标，
Item=(String,Any)，next 返回 IterationStep((String,Any))，重复耗尽持续 done。
for 的 key 因此是 String；错误使用为 i64 在执行 body 前拒绝且不输出产物。
顺序不保证，快照成本 O(capacity+entries)，不是无限制泛型集合或任意 Hash 键。

VM 的 map_keys 复用 occupied_buckets 校验 state 和 occupied count，并检查键
的 String 长度和 UTF-8。分配 List 前后分别取得 MapLayout，不持有跨分配裸布局；
Map 和 List 位于临时根域。复制期间无托管分配或用户回调，BuiltinCtx 直接发布
受检结果。占用桶共享遍历也用于原 Map Display，不重复定义状态规则。

新增 native ID126 __map_keys、builtin ABI8。旧1–6阈值不变，旧7只接受ID<126。
安装新helper必须具备Map、MapBuffer、List、Buffer四个角色；ID/name、缺角色与
旧1–7声称新helper均拒绝。Map布局、TPOL、NSAM、外层NSBC未改变。旧Display
ABI6夹具直接构造原始schema及Value入口，真实CLI仍输出 Raw { value: 42 }。

VM覆盖8192满容量、碰撞、tombstone、坏state/occupied count/String长度/UTF8；
native覆盖错误arity和非Map。GC回归在暂停前创建两个独立MapIterator，丢弃原Map，
实际收集至少4次，两次continuation恢复分别消费独立游标、合计42且栈全部释放。
continuation不会深复制共享heap游标，测试明确创建两个游标。新归档回归删除源码
后启动独立CLI，验证snapshot mutation、null、准确step反射和重复done。

root负责VM/native/ABI/std与归档、GC验证；测试写入者只在
/tmp/nessa-map-iteration-test-worker新增map_iteration_tests.rs，交付9组实际通过
的行为测试，root读回后合并并复跑。只读调查者提供布局/root/ABI证据，运行记录
在 /tmp/nessa-map-iteration-graph.json。本轮使用graph-engineering-workflow；
graphify缺少本地CLI和现成图而未运行。没有提交、推送、部署、发布或对外发送。

日志使用 /tmp/nessa-map-iteration- 前缀：vm、loader、native、gc、target、
legacy-display、full、fmt、check、clippy、diff。新上下文的独立复核实际重跑
全workspace：1279 passed、0 failed、1个原有ignored；fmt、全仓check、严格
Clippy和diff检查全部exit0。独立日志在
/tmp/nessa-map-grader-{full,fmt,check,clippy,diff}.log。

固定C1–C12最终12/12通过，第一轮gate=passed；无不适用、失败或不可达项。
C1/C2/C7/C8为记录的attested结论，其余由独立源码/产物检查或实际重跑取得。
审查者对照隔离副本的原始tar，确认357个原文件未改且仅新增交付测试，该文件
与合并后版本完全一致。修复循环处理了旧ABI夹具问题和记录波次字段缺口；
补注披露来源与补注时间，不伪造历史事件。修复后完整rubric已复核。

实际4个worker/上限4、峰值并发2/上限4、2波/上限3、0重试/上限1、深度1/
上限1、4个worker turn/上限6、1轮grader/上限2，用时不足35分钟。预算按本机
可观察的worker turn执行。无待解冲突，也未请求或执行不可逆对外操作。
完整项目目标保持active，动态carrier的能力传输及关联视图语义待用户选择；
泛型集合、Hash和其他类型系统/标准库设计仍需继续。
