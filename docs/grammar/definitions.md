# 定义 (Definitions)

## 统一可见性前缀

```ebnf
visibility_modifier -> pub | private

definition -> visibility_modifier? definition_core
definition_core ->
	function_def |
	effect_def |
	struct_def |
	enum_def |
	trait_def |
	impl_def |
	impl_trait_def |
	derive_def |
	typealias |
	newtype |
	module_def |
	const_decl |
	global_decl |
	assoc_decl |
	use_statement
```

## 函数与效果

```ebnf
-- 函数是没有async的
function_def -> fn id ((param*)) (-> expr)? (handles expr)? ((= expr) | block)
effect_def -> async? effect id (effect_param*) (-> expr)?
```

## 结构与枚举

```ebnf
struct_def -> struct id { (struct_field | statement)* }
struct_field -> id : expr (= expr)?

enum_def -> enum id { (enum_variant | statement)* }
enum_variant -> id ((param*))?
```

## Trait 与实现

```ebnf
trait_def -> trait id ((expr+))? { (trait_def_fn | trait_derive_fn | statement)* }
trait_def_fn -> def fn id ((param*))? (-> expr)? (handles expr)?
trait_derive_fn -> derive fn id ((param*))? (-> expr)? (handles expr)? ((= expr) | block)

impl_def -> impl expr { statement* }
impl_trait_def -> impl expr for expr { statement* }
extend_def -> extend expr { statement* }
extend_trait_def -> extend expr for expr { statement* }
derive_def -> derive expr* for expr
```

## 类型定义

```ebnf
typealias -> typealias id = expr
newtype -> newtype id = expr
```

## 模块与导入

```ebnf
module_def -> mod id { statement* }

use_statement -> pub? use path
path ->
	path -> id |
	path_projection |
	path_projection_multi |
	path_projection_all |
	super_path |
	package_path |
	path_as_bind

path_projection -> path . path
path_projection_multi -> path . { path* }
path_projection_all -> path . *
super_path -> . path
package_path -> @ path
path_as_bind -> id as id
```

说明：

- `pub use ...` 用于重导出（re-export）。

## 参数语法

```ebnf
param ->
	param_optional |
	param_typed |
	param_self |
	param_varargs |
	param_lambda

param_optional -> . id : expr = expr
param_typed -> pattern : expr
param_self -> self
param_varargs -> ...id (: expr)?
param_lambda -> lambda pattern (: expr)?

effect_param -> param | param_catch
param_catch -> catch id (: expr)?
```

`catch` 参数仅用于效应声明，同一效应最多有一个。它绑定运行时捕获的
continuation，不是效应调用者传入的参数；其类型为 `Continuation`，省略类型
注解时使用该类型，显式注解也必须指向该类型。

## 可见性与关联性关键字

- `pub`、`private` 与 `assoc` 已在词法层作为保留关键字。
- `assoc_decl -> assoc id : expr = expr`。
- `assoc_decl` 仅允许出现在 `trait`、`impl`、`extend` 的语句体中。
- 此语法构建独立关联绑定AST；Type关联声明支持具体trait实现绑定和签名替换，
  关联常量及动态关联trait视图仍明确拒绝，不将其降为普通常量。
