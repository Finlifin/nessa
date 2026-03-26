# 异步代数效应 (Asynchronous Algebraic Effects)

Nessa不仅提供了健全的标准Algebraic Effect，还提供了更强大的异步版本。

```nessa
mod http:
    async effect get(url: String) -> !NetErr Response 
    async effect post(url: String, body: String) -> !NetErr Response 

fn some() -> #http.get Unit:
    -- promise: Promise
    let promise = http.get("localhost:8000/hellowolrd")
    println("Waiting for response from the server...")
    promise.await match
        response! => println(response.body)
        error e => println(e)

some()# {
    -- async pattern，表面该函数将在新的task中异步执行
    async http.get(url) => some_lib.fetch(.GET, ...)
}
    
{-
    得益于nessa的抢占型并发模型，async effect的实现较为直观。
    handler在接受到effect call后，会spawn一个子task，负责执行handler中对应的逻辑（如`some_lib.fetch(...)`），
    而后将改task的promise注册进本task的promise集中，并立即返回一个promise handle。
    在子task完成时，将会中断本task，将结果填入promise集中对应的promise。
    而需要async effect call结果的上下文，需要显示地await promise，如果该promise还未就绪，
    该task将被挂起，直到有中断到达该task。
    - task是并发执行的调度单位
    - 每个effect call都有其独立id
    - nessa会在safe point检查是否有中断到达
    - task树是结构化并发的
-}
```



```nessa
-- 定义一个异步 effect
async effect fetch_image(url: String) -> Image
-- 也许可以直接用continuation来直接表达组件实例
-- 对于这个组件的渲染周期，我们期待它被执行两次，
-- 一次是初次执行该函数时，
-- 二是image fetch完成时，我们resume该continuation, 再次重新render
fn BubbleFragment(url: String) -> #component.effects #fetch_image Component {
    let component_base = ComponentBase()
    let image = Image.from_color(.black, 100, 100)
    let fetched = false
    
    while not component_base.unmounted:
        render {
            Img {
                src: image,
                style: {
                    -- 闪动特效
                    `effect`: if fetched { .flash } else { .none }
                }
            }
        }#
        image = fetch_image(url).await match 
            img! => img
            error reason => 
                Log.d("$reason")
                Image.load_failed

}

```
