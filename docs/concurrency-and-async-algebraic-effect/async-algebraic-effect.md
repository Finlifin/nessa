# 异步代数效应 (Async Algebraic Effects)

异步代数效应是 nessa 中实现并发的唯一机制。通过 `async effect` 声明一个异步操作，handler 会在独立的子 task 中执行，而 effect 调用立即返回一个 `Future`。

## 声明

使用 `async effect` 关键字声明异步效应：

```nessa
mod http:
    async effect get(url: String) -> !NetErr Response
    async effect post(url: String, body: String) -> !NetErr Response
```

与同步效应的区别仅在于 `async` 前缀。参数和返回类型的语法完全一致。

### 语法参考

```ebnf
effect_def -> async? effect id (param*) (-> expr)?
```

## 调用与 Future

调用 async effect 使用与同步效应相同的 `#` 传播语法，但返回的是一个 `Future`：

```nessa
fn fetch_data() -> #http.get Unit {
    -- http.get(...)# 返回 Future，不会阻塞
    let future = http.get("localhost:8000/helloworld")#

    -- 可以在等待结果的同时执行其他逻辑
    println("Waiting for response from the server...")

    -- 显式 await 以获取结果
    future.await match {
        response! => println(response.body),
        error e => println(e),
    }
}
```

### 执行流程

```
[调用 task]                           [子 task]
    │                                     │
    ├─ http.get(url)# ──────────────► spawn 子 task
    │    └─ 返回 Future                   │
    │                                      │ 执行 handler 逻辑
    ├─ println("Waiting...")              │ (如 some_lib.fetch(...))
    │                                      │
    ├─ future.await ──────────────────► 等待子 task 完成
    │    (task 可能被挂起)                 │
    │                                      │ 完成，结果填入 Promise
    ├─ ◄──────────────────────────── 中断，恢复 await
    │    └─ 获得 Response
    ▼
```

## Handler 语法

async handler 使用 `async` 前缀的 case arm：

```nessa
fetch_data()# {
    -- async 前缀表示该 handler 将在新的子 task 中异步执行
    async http.get(url) => some_lib.fetch(.GET, url),
    async http.post(url, body) => some_lib.fetch(.POST, url, body),
}
```

### Handler 的执行方式

在收到 async effect call 后：

1. **spawn 子 task**：为该 handler 逻辑创建一个新的子 task
2. **注册 Promise**：将子 task 的 promise 注册到当前 task 的 promise 集中
3. **立即返回 Future**：effect 调用点不阻塞，立刻得到一个 `Future` 引用
4. **子 task 执行**：handler 逻辑（如 `some_lib.fetch(...)`）在子 task 中并发执行
5. **完成通知**：子 task 完成时，中断父 task，将结果填入对应的 promise

这与同步效应的 [in-place handler application](../algebraic-effect-and-multi-prompt-delimited-continuation/in-place-handler-application.md) 形成鲜明对比——async handler **不会**在当前上下文中 in-place 执行。

## Future 与 Await

### `Future` 类型

`Future` 是一个内建类型，表示一个异步计算的惰性结果引用：

```nessa
let future: Future<!NetErr Response> = http.get(url)#
```

### `.await` 操作

使用 `.await` 来等待 future 的结果：

```nessa
let result = future.await   -- result: !NetErr Response
```

`.await` 的行为：
- 如果 future 已就绪，立即返回结果
- 如果 future 未就绪，**挂起当前 task**，直到有中断到达（子 task 完成、被其他事件唤醒等）

### 并发 Await

可以同时发出多个 async effect call，然后选择性地 await：

```nessa
fn fetch_multiple() -> #http.get Unit {
    -- 并发发出三个请求
    let f1 = http.get("/api/users")#
    let f2 = http.get("/api/posts")#
    let f3 = http.get("/api/comments")#

    -- 三个请求在三个子 task 中并发执行
    -- 可以按任意顺序 await
    let users = f1.await
    let posts = f2.await
    let comments = f3.await
}
```

## Promise 集与中断机制

### Promise 集

每个 task 内部维护一个 **promise 集** (promise set)——所有由该 task 发出的 async effect call 产生的 promise 的集合：

```
Task A
├── promise_set:
│   ├── #1: http.get("/users")  → 子 Task B
│   ├── #2: http.get("/posts")  → 子 Task C
│   └── #3: http.get("/comments") → 子 Task D
```

每个 effect call 都有**独立的 ID**，用于在 promise 集中唯一标识。

### 中断机制

nessa 使用基于 **safe-point** 的抢占式调度模型：

1. 编译器在**循环回边**和**函数调用点**插入 safe-point 检查
2. 在 safe-point，task 检查是否有待处理的中断
3. 子 task 完成时，会向父 task 发送中断，将结果填入 promise

```nessa
fn compute_while_waiting() -> #http.get Unit {
    let future = http.get("/slow-api")#

    -- 在子 task 执行 HTTP 请求的同时，继续执行计算
    for i in 0..1000000 {
        -- [safe-point] 编译器在循环回边插入检查
        -- 如果子 task 完成，中断到达，promise 被填充
        heavy_computation(i)
    }

    -- 此时 future 可能已经就绪
    let result = future.await
}
```

### Safe-Point 插入位置

编译器在以下位置自动插入 safe-point：

| 位置 | 说明 |
|------|------|
| 循环回边 | `while` / `for` 循环每次迭代前 |
| 函数调用 | 每次函数调用前后 |

## 错误处理

async effect 的返回类型可以包含 error qualified type：

```nessa
async effect fetch(url: String) -> !NetErr Response
```

在 await 时，错误可以通过标准的 `!` 机制传播或处理：

```nessa
fn fetch_or_default(url: String) -> #fetch Response {
    let future = fetch(url)#

    future.await! {
        ok! => response,
        NetErr.Timeout(_) => Response.empty(),
        NetErr.* as e => {
            log("fetch failed: {e}")#
            Response.empty()
        }
    }
}
```

## 完整示例

```nessa
mod http:
    async effect get(url: String) -> !NetErr Response
    async effect post(url: String, body: String) -> !NetErr Response

fn fetch_user_data(user_id: String) -> #[http.get, log] !NetErr UserData {
    -- 并发获取用户信息和用户帖子
    let user_future = http.get("/api/users/{user_id}")#
    let posts_future = http.get("/api/users/{user_id}/posts")#

    log("fetching data for user {user_id}")#

    -- 等待两个结果
    let user_resp = user_future.await!
    let posts_resp = posts_future.await!

    UserData {
        user: user_resp.parse(.json, User)!,
        posts: posts_resp.parse(.json, List)!,
    }
}

fn main() {
    let result = fetch_user_data("alice")# {
        async http.get(url) => reqwest.get(url),
        async http.post(url, body) => reqwest.post(url, body),
    }# {
        log(msg) => println("[LOG] {msg}"),
    }

    result match {
        data! => println("Got: {data}"),
        error e => println("Error: {e}"),
    }
}
```

这个例子展示了 async effect 的核心价值：

1. `fetch_user_data` 不关心 HTTP 请求如何执行——可以用真实网络库，也可以用 mock
2. 并发性是自然的——两个 `http.get` 调用的 handler 在各自的子 task 中并发执行
3. 错误处理与同步代码完全一致——使用 `!` 传播和消除
4. 同步效应 (`log`) 和异步效应 (`http.get`) 可以自然组合
