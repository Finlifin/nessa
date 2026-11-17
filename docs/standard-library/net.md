# `std.net` — 跨平台非阻塞网络库

`std.net` 在运行时专职 I/O Reactor（Unix 基于 `libuv`，Windows 10 1709+ 基于 Winsock2 `WSAPoll`）之上提供非阻塞 TCP、UDP 与异步 DNS 解析接口：

- 所有连接、监听接收、流式读写、数据报收发与主机名解析仅挂起当前 `Task`，不会阻塞计算 Worker 线程；
- 错误统一返回 `std.io.IoError` 受检错误集（`!IoError`）；
- **取消零泄漏与零丢包保证**：
  - 当 `TcpListener.bind`、`TcpStream.connect` 或 `UdpSocket.bind` 在等待期间或刚完成瞬间遭遇任务取消时，底层新建套接字由 Reactor 自动关闭回收，绝不泄漏系统端口或文件描述符；
  - 当 `TcpListener.accept` 或 `UdpSocket.recv_from` 被取消时，未交付给调用方的新连接/数据报自动重回监听队列，供后续接收者继续读取；
  - 当 `TcpStream.read_bytes` / `read_line` 被取消时，Reactor 在校验未取消状态后才消费内部读缓冲区，绝不吞掉已到达的对端字节流；
  - 配合 `defer { stream.close() }` / `defer { listener.close() }` 可在正常退出、错误返回与取消展开时确定性释放已获取句柄。

## 地址与主机名解析

### `SocketAddr`

```nessa
pub struct SocketAddr {
    pub host: String,
    pub port: i64,

    pub fn new(host: String, port: i64) -> SocketAddr
}
-- 实现 Eq, PartialEq, Display（IPv6 自动格式化为 [host]:port）
```

### `resolve_host`

```nessa
pub fn resolve_host(host: String) -> !IoError List
```

通过 `uv_getaddrinfo` 异步解析主机名，返回去重后的 IP 地址字符串列表（`List` of `String`，覆盖 IPv4 与 IPv6）。

## TCP 网络编程（`TcpListener` & `TcpStream`）

### `TcpListener`

```nessa
impl TcpListener {
    pub fn bind(host: String, port: i64, .backlog: i64 = 128) -> !IoError TcpListener
    pub fn accept(self) -> !IoError TcpStream
    pub fn local_addr(self) -> !IoError SocketAddr
    pub fn close(self) -> Unit
}
```

- 传入 `port = 0` 可由操作系统分配临时空闲端口，随后通过 `listener.local_addr()!.port` 读取实际监听端口。
- `accept()` 在连接到达前挂起当前任务；若已排队的连接到达则立即返回 `TcpStream`。

### `TcpStream`

```nessa
impl TcpStream {
    pub fn connect(host: String, port: i64) -> !IoError TcpStream

    pub fn read_bytes(self, .max_bytes: i64 = 65536) -> !IoError List
    pub fn read_to_string(self, .max_bytes: i64 = 65536) -> !IoError String
    pub fn read_line(self) -> !IoError ?String

    pub fn write(self, data: String) -> !IoError i64
    pub fn write_str(self, data: String) -> !IoError i64
    pub fn write_line(self, line: String) -> !IoError i64
    pub fn write_bytes(self, bytes: List) -> !IoError i64

    pub fn set_nodelay(self, enable: bool) -> !IoError Unit
    pub fn local_addr(self) -> !IoError SocketAddr
    pub fn peer_addr(self) -> !IoError SocketAddr
    pub fn close(self) -> Unit
}
```

- **EOF 语义**：对端正常关闭连接时，`read_bytes` 返回空 `List`（`len == 0`），`read_to_string` 返回 `""`，`read_line` 返回 `null`（若关闭前仍有未以 `\n` 结尾的残留字节，则先返回最后一行，下次调用返回 `null`）。
- `read_line` 自动剥离行尾 `\n` 或 `\r\n`。

## UDP 网络编程（`UdpSocket` & `UdpDatagram`）

```nessa
pub struct UdpDatagram {
    pub data: List,
    pub addr: SocketAddr,

    pub fn text(self) -> String
}
-- 实现 PartialEq, Display

impl UdpSocket {
    pub fn bind(host: String, port: i64) -> !IoError UdpSocket
    pub fn send_to_str(self, host: String, port: i64, data: String) -> !IoError i64
    pub fn send_to_bytes(self, host: String, port: i64, bytes: List) -> !IoError i64
    pub fn recv_from(self, .max_bytes: i64 = 65535) -> !IoError UdpDatagram
    pub fn local_addr(self) -> !IoError SocketAddr
    pub fn close(self) -> Unit
}
```

## 并发回显服务器示例

```nessa
use std.io.IoError
use std.net.{TcpListener, TcpStream}

async effect serve_once(listener: TcpListener) -> !IoError String

fn main() -> !IoError Unit {
    let listener = TcpListener.bind("127.0.0.1", 0)!
    defer { listener.close() }
    let port = listener.local_addr()!.port

    handles async serve_once(srv: TcpListener) => {
        let conn = srv.accept()!
        defer { conn.close() }
        let line = conn.read_line()! match {
            s? => s,
            null => "",
        }
        conn.write_line("echo:" ++ line)!
        line
    }

    let server_fut = serve_once(listener)#

    let client = TcpStream.connect("127.0.0.1", port)!
    defer { client.close() }
    client.write_line("hello nessa")!
    let reply = client.read_line()!

    assert_eq(server_fut.await!, "hello nessa")
    assert_eq(reply, "echo:hello nessa")
}
```
