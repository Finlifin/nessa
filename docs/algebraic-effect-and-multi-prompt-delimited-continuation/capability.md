# Capability — 类型实例作为 Handler

Capability 机制允许类型实例充当效应的 handler。通过在 `impl` 块中定义带 `handles` 标注的方法，一个类型实例可以声明它拥有处理某种效应的**能力**。

## 基本概念

Capability 将 handler 逻辑封装在类型实例中，使得：

1. handler 可以携带状态（通过 `self`）
2. handler 可以被复用和组合
3. handler 的生命周期与实例绑定

## 定义 Capability

在 `impl` 块中，使用 `handles(effect_name)` 标注一个方法，使其成为某个效应的 handler：

```nessa
effect log(msg: String, level: LogLevel) -> Unit

struct Logger {
    prefix: String,
    min_level: LogLevel,
}

impl Logger {
    -- 处理 self 参数外的其他参数，以及返回类型
    -- 要匹配对应效应的类型签名
    pub fn log_handler(self, msg: String, level: LogLevel) -> Unit handles(log) {
        if level >= self.min_level {
            println("{self.prefix} [{level}]: {msg}")
        }
    }
}
```

### 签名匹配规则

capability 方法必须满足：
- 第一个参数为 `self`
- 其余参数的类型与对应 effect 的参数类型匹配
- 返回类型与 effect 的返回类型匹配
- 使用 `handles(effect_name)` 标注声明所处理的效应

## 使用 Capability

### 方式一：通过 `.use()` 显式应用

将 capability 方法作为 handler 传递给 `.use()`：

```nessa
fn foo() -> #log Unit {
    log("starting", LogLevel.info)#
    -- ... 一些操作 ...
    log("done", LogLevel.debug)#
}

fn main() {
    let logger = Logger { prefix: "[APP]", min_level: LogLevel.info }
    foo().use(logger.log_handler)
}
```

`logger.log_handler` 是一个方法引用，绑定了 `self = logger`。它可以直接被 `.use()` 用作 handler，因为除去 `self` 后的签名与 `log` 效应的签名匹配。

### 方式二：通过 `handles` 绑定到作用域

使用 `handles(effect_name) let` 语法，将一个实例绑定为其后词法作用域中某个效应的 handler：

```nessa
fn main() {
    -- 后续词法作用域内传播出来的 log effect call 都会自动使用 logger
    handles(log) let logger = Logger { prefix: "[APP]", min_level: LogLevel.debug }

    -- 但是效应仍然需要用 # 传播出去
    foo()#
    bar()#
}
```

`handles(effect_name) let` 做了两件事：
1. 绑定变量 `logger`
2. 在该作用域内，将 `logger` 注册为 `log` 效应的 handler

> **注意**：`handles(log) let` 要求该类型有且仅有一个 `handles(log)` 的方法，否则会产生歧义。

### 方式三：在消除块中手动调用

capability 方法本身也是一个普通的方法，可以在消除块中手动调用：

```nessa
fn main() {
    let logger = Logger { prefix: "[MANUAL]", min_level: LogLevel.info }

    foo()# {
        log(msg, level) => {
            logger.log_handler(msg, level)
        }
    }
}
```

这种方式最为灵活，但也最冗长。

## 多 Capability 组合

一个类型可以实现多个效应的 capability：

```nessa
effect read_line() -> String
effect print_line(msg: String) -> Unit

struct Terminal {
    encoding: String,
}

impl Terminal {
    pub fn read(self) -> String handles(read_line) {
        stdin.read_line(self.encoding)
    }

    pub fn print(self, msg: String) -> Unit handles(print_line) {
        stdout.write(msg ++ "\n", self.encoding)
    }
}

fn main() {
    let term = Terminal { encoding: "utf-8" }

    -- 使用 .use() 分别绑定
    interactive_app()
        .use(term.read)
        .use(term.print)
}
```

也可以使用 `handles` 绑定：

```nessa
fn main() {
    handles(read_line) handles(print_line)
        let term = Terminal { encoding: "utf-8" }

    interactive_app()#
}
```

## Capability 与 Trait 的关系

Capability 可以与 trait 结合使用，定义一组效应处理的接口契约：

```nessa
trait Storage {
    def fn load(self, key: String) -> ?String handles(storage_load)
    def fn save(self, key: String, value: String) -> Unit handles(storage_save)
}

struct FileStorage { base_path: String }
struct MemoryStorage { data: Map }

impl Storage for FileStorage { ... }
impl Storage for MemoryStorage { ... }
```

这使得不同的存储后端可以作为同一组效应的 handler，在编译时或运行时灵活切换。

## 语法参考

```ebnf
capability_method -> fn id (self, param*) (-> expr)? handles (id) ((= expr) | block)
handles_let       -> handles (id) let pattern = expr
handler_application -> expr.use(expr)
```