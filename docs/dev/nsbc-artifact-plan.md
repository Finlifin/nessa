# NSBC 自包含产物实施状态与后续计划

本切片已接通单个编译产物在新进程中的独立加载执行。跨包链接、增量装载和
TypeIndex 重定位仍属后续工作，不能据此认定整个 NSBC 包系统已经完成。
具体容器格式和兼容规则见 [archive.md](nessa-bytecode/archive.md)。

## 当前实现（2026-10-07）

- `nsbc::CompiledArtifact` 保存 CodegenOutput、完整 TypePool、显式入口以及
  builtin ABI revision 和所用 ID/名称清单。`write_artifact` / `read_artifact`
  编解码完整产物；低层 `write_archive(CodegenOutput)` 缺少类型池和入口，仍
  拒绝非空 globals，不能用它替代完整产物 API。
- TypePool snapshot/restore 按原 TypeIndex 保留 TypeInfo、structural_types、
  methods、trait_impls、vtables、well_known 和 null_type，恢复后重建查询缓存。
  结构来源不由 TypeKind 猜测，不合并同形状的名义 effect。
- 类型池 codec 使用版本化 payload 和显式 little-endian tag/字段。函数、类型、
  字段、variant、method 和 associated type 名称保存内联 UTF-8，读取后重新
  intern；writer 使用受检字符串查询，不直接保存进程 StrId。
- CODE 的函数表保存 section-relative offset、寄存器/参数数量和 closure flag；
  METADATA 保存名称、function_type、globals 的类型/可变性与显式 entry。
  已执行的 globals 值和 initialized 状态不落盘。初始化顺序已在 bootstrap
  字节码中，加载器不再次规划初始化，启动入口按保存的 FuncId 执行。
- 方法名使用专用重定位记录。序列化时将方法调用改为可重定位的 far 形式，
  受检调整受影响的常量引用、跳转和 safepoint PC；加载后按名称内容重建 StrId。
  普通 UInt 常量不作为名称重写，DERIVE_FUNC_ID 保持合法的派生方法 sentinel。
- STACK_MAPS 保存 safepoint PC；METADATA 明确 tagged-root 扫描模式，当前 VM
  扫描 TaggedValue 根。这不是按活跃变量/type bitmap 生成的精确 stackmap。
- 容器 header 为60字节，按实际 offsets/padding 读写。writer 对 header 之后
  的全部字节计算 SHA-256，reader 核对非零 checksum；完整 artifact 要求非零
  checksum。低层容器保留检查旧零 checksum 文件的兼容路径，不能直接执行。
- 执行加载校验 target 与当前平台的兼容性，拒绝损坏范围、重复 section、未知
  flags、非法 UTF-8、tag/count/索引、类型环和尾随数据。文件上限64MiB，类型池
  codec 累计向量元素上限262144，透明类型/trait 查询深度上限256。

## 加载前验证与运行安装

`validate_artifact` 同时用于源码内存产物和读取产物，检查类型池、函数 ID/签名、
closure 捕获布局、参数/寄存器/槽容量、常量/Type/global 引用、指令编码、跳转目标、
调用目标、方法/vtable 函数引用及无参非 closure 入口。派生方法 sentinel 被单独
处理；不支持的执行形式明确拒绝，不能把无效产物交给解释器后再碰运气。

builtin 指针不落盘。产物记录 ABI revision 与实际 CallBuiltin 使用的 ID/名称，
共用安装器在修改 VM 状态前核对 runtime catalog；不兼容 revision、伪造 ID/名称
和未声明 builtin 调用报错。typed native 的源码签名由 adapter closure 保留，
没有给 native 注册表添加重复静态签名。

`driver::install_artifact` 是源码和归档共用安装路径，顺序为类型池、global schema、
函数和常量，再创建保存入口的根 task。CLI `build` 写完整 artifact；
`nessa run program.nsbc` 按二进制读取并加载，源码 `run` 仍先编译。归档输入不
接受源码 AST dump；损坏或不兼容产物返回错误和非零退出状态。

## List 角色与 builtin ABI 兼容

动态 List/Buffer 使用版本1的稳定保留 TypeId 与既有 Struct descriptor，
TPOL1 编码不变。新池在原 intrinsic/trait/null 前缀后追加角色；旧池按原索引
恢复，不注入或移动用户类型。角色可缺省但必须同时缺省；出现时检查唯一性、
nominal kind、精确字段顺序/类型/偏移/大小/对齐，未知保留身份明确拒绝。
普通 NewObject（含 alias）不能创建这些角色；字段操作由 VM 拒绝。
NewList/LoadIndex/StoreIndex 受检支持，集合opcode缺角色拒绝，NewMap仍拒绝。

builtin ABI 已升为2：ID100 __list_init真实返回List，ID101–105为新增操作。
共用安装器显式接受 revision1 且仅导入旧ID小于100的产物；revision1引用100
或任何新ID拒绝。revision2 List imports须有完整角色。ID/名称仍逐项核对，
未知revision、损坏布局与伪造manifest在执行entry之前失败。

## 已验证行为

- snapshot→restore 后结构驻留复用原索引，名义 effect 保持独立；合法名义
  递归结构接受，alias/trait/透明结构环及损坏内建前缀、布局、名称与索引拒绝。
- 类型池所有 TypeKind、TypeId、布局、methods、trait impl 和 vtable 字段往返；
  截断、非法 tag/count/UTF-8、越界索引与尾随数据返回错误，不 panic。
- 独立进程回归在编译后删除源码，再只凭产物执行，覆盖初始化、alias/Type、
  捕获 closure、128-bit 数值、derived 方法、EFFECT 和长距离跳转。加载进程先
  驻留5000个无关字符串，验证函数/类型/方法名称不依赖原进程 StrId。
- driver/CLI 的归档执行及 builtin manifest 拒绝回归已通过；全仓验证的最终
  结果另见 [implementation-progress.md](implementation-progress.md)。

## 仍需落实

- 跨包 imports/exports、package identity、链接和 TypeIndex 重定位、增量装载。
- 稳定 TypeId 算法：当前部分类型仍为 ZERO。restore 不按 ZERO 合并名义类型；
  非零 identity 冲突拒绝，只有 canonical 相同的透明 alias 可共享 identity。
  保存现有 TypeId 不等于完成跨编译稳定身份。
- 精确 stackmap、DEBUG_INFO、文本字节码格式、WASM 与压缩等可选功能；完整
  artifact reader 当前只接受已实现的 CODE/METADATA/CONSTANTS/STACK_MAPS。
- 已知函数声明与直接 lambda 的 optional/default/named 调用已生成完整固定实参，
  单 List 变参也已在源码端打包为固定List槽，归档直接执行同一布局；函数值
  默认/变参声明元数据、一般方法参数绑定、双变参、函数variance、携带值enum/
  newtype及完整trait系统仍需实现。非法缺参在编译期拒绝。
- 更完整的恶意输入资源分析、跨平台矩阵和性能验证。
  当前机器验证不代表所有 target 的执行兼容性。

原生 SP/FP continuation 栈切换、语言对象最后引用后的模板回收和完整移动 GC
仍属原项目目标，不因自包含 artifact 执行通过而完成。
