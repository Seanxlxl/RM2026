//! HTTP 适配层：Rocket 接收请求 → 校验路由 → 异步读取请求体 → 解析 JSON
//! → 在阻塞线程中执行业务 → 把业务结果转换为 HTTP 响应。
//! Rocket 负责监听 TCP、解析 HTTP 和发送响应，这里无需手写网络协议。

// crate 表示当前库；导入业务状态、路由表和错误辅助函数。
use crate::{ROUTES, Service, error, route_error};
// 扩展数字的方法，让 512.kibibytes() 表示 512 × 1024 字节。
use rocket::data::ToByteUnit;
// Fairing 是生命周期钩子；Info/Kind 指定名称及要接收哪些事件。
use rocket::fairing::{Fairing, Info, Kind};
// Method 表示 HTTP 方法；Status 表示 HTTP 状态码。
use rocket::http::{Method, Status};
// Handler 定义请求处理接口；Outcome 是交给 Rocket 的处理结果。
use rocket::route::{Handler, Outcome};
// Json 包装响应数据，负责序列化 JSON 并设置对应的 Content-Type。
use rocket::serde::json::Json;
// Build/Orbit 是应用的阶段类型；Data 是请求体流，Request/Response 是请求/响应。
use rocket::{Build, Data, Orbit, Request, Response, Rocket, Route};
use serde_json::Value;
// Arc 是线程安全的共享指针：clone 增加引用计数，不复制整个 Service。
use std::sync::Arc;
// 单调时钟，用于测量经过时间，不受系统时间校准的影响。
use std::time::Instant;

// 不保存字段的结构体；它只承担输出启动信息和请求日志的职责。
struct ConsoleOutput;

// 宏把异步 trait 方法转换为 Rocket 所需的形式。
#[rocket::async_trait]
impl Fairing for ConsoleOutput {
    // 告诉 Rocket 这个钩子叫什么、关注哪些事件；info 本身不需要异步。
    fn info(&self) -> Info {
        Info {
            name: "Console output",
            // | 合并事件标志：启动完成、收到请求、生成响应。
            kind: Kind::Liftoff | Kind::Request | Kind::Response,
        }
    }

    // Orbit 表示应用已经启动；Rocket 会在准备好接收请求后调用此方法。
    async fn on_liftoff(&self, rocket: &Rocket<Orbit>) {
        // 从生效的配置中读取监听地址；eprintln! 向标准错误输出日志。
        let address = std::net::SocketAddr::new(rocket.config().address, rocket.config().port);
        eprintln!("Listening on http://{address}");
        eprintln!("Press Ctrl+C to exit. All in-memory data is lost on exit.");
        eprintln!("Routes:");
        // 遍历业务路由表，打印目前实际支持的接口。
        for (method, path) in ROUTES {
            eprintln!("  {method} {path}");
        }
    }

    // 请求开始时记录时间，供响应日志计算本次请求的耗时。
    // '_ 让编译器推断生命周期；&mut 是可变借用，_ 参数表示不使用请求体。
    async fn on_request(&self, request: &mut Request<'_>, _: &mut Data<'_>) {
        // 为这一次请求缓存开始时间。传入函数 Instant::now，首次访问时才调用。
        request.local_cache(Instant::now);
    }

    // 'r 是生命周期名称，约束请求与响应引用的有效时间；不是运行时参数。
    async fn on_response<'r>(&self, request: &'r Request<'_>, response: &mut Response<'r>) {
        // 同类型缓存已经存在，因此这里取到之前的开始时间，再计算耗时。
        let elapsed = request.local_cache(Instant::now).elapsed();
        // 输出方法、路径、状态码、耗时；{:.1} 表示保留一位小数。
        eprintln!(
            "{} {:?} -> {} {:.1}ms",
            request.method(),
            request.uri().path().as_str(),
            response.status().code,
            elapsed.as_secs_f64() * 1000.0, // 秒换算为毫秒。
        );
    }
}

// 自动生成 clone；复制 Dispatch 时只复制内部 Arc 的共享引用。
#[derive(Clone)]
// 元组结构体只有一个字段，通过 self.0 访问；所有处理器共享同一份业务状态。
struct Dispatch(Arc<Service>);

#[rocket::async_trait]
impl Handler for Dispatch {
    // Rocket 为匹配的请求调用 handle；请求体 Data 移入此函数，由它负责读取。
    async fn handle<'r>(&self, request: &'r Request<'_>, data: Data<'r>) -> Outcome<'r> {
        // 从请求中取出方法和路径，再转为拥有数据的 String。
        // 后面要移到另一个线程，不能把依赖本次 Request 的借用带过去。
        let method = request.method().as_str().to_owned();
        // 这里只取路径部分，不包括 ? 后面的查询参数。
        let path = request.uri().path().as_str().to_owned();
        // 尽早拒绝未知路径或不支持的方法，不必读取请求体和执行业务。
        if let Some(status) = route_error(&method, &path) {
            // Outcome::from 把（状态码，JSON 响应）转换为 Rocket 的处理结果。
            return Outcome::from(
                request,
                (
                    Status::new(status), // 把业务层的 u16 转为框架的状态码类型。
                    Json(
                        error(
                            status,
                            if status == 404 {
                                "Not found"
                            } else {
                                "Method not allowed"
                            },
                        )
                        .1, // error 返回元组；这里只取 JSON，状态码已在上面设置。
                    ),
                ),
            );
        }
        // 请求头可能缺失；缺失时使用空字符串，再交由业务层判断是否需要登录。
        let authorization = request
            .headers()
            .get_one("Authorization")
            .unwrap_or("")
            .to_owned();
        // if 表达式产生一个 Value；当前仅为 POST/PUT 读取并解析请求体。
        let body = if matches!(request.method(), Method::Post | Method::Put) {
            // 限制读取量；into_bytes 异步收集网络数据，需要等待时让出运行时线程。
            // .await 得到 Result；这不同于阻塞当前线程直到所有字节到齐。
            let bytes = match data.open(512.kibibytes()).into_bytes().await {
                // 完整读完，且没有超过读取限制，才接受这份请求体。
                Ok(bytes) if bytes.is_complete() => bytes,
                // 能读取但不完整，说明超出上限；拒绝截断的 JSON，返回 413。
                Ok(_) => {
                    return Outcome::from(
                        request,
                        (
                            Status::PayloadTooLarge,
                            Json(error(413, "Request body too large").1),
                        ),
                    );
                }
                // 底层读取失败，返回 400；不继续尝试解析残缺请求体。
                Err(_) => {
                    return Outcome::from(
                        request,
                        (
                            Status::BadRequest,
                            Json(error(400, "Cannot read request").1),
                        ),
                    );
                }
            };
            // bytes 借用为字节切片；::<Value> 指定解析结果的类型为通用 JSON 值。
            // JSON 解析仍是同步工作，并不会因为外层函数是 async 就自动让出线程。
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(body) => body,
                // 非 UTF-8 或不合法的 JSON 都无法解析，返回 400。
                Err(_) => {
                    return Outcome::from(
                        request,
                        (
                            Status::BadRequest,
                            Json(error(400, "Expected UTF-8 JSON").1),
                        ),
                    );
                }
            }
        } else {
            // GET/DELETE 等没有要解析的请求体，用 Null 作业务参数占位。
            Value::Null
        };
        // 密码计算和 std::sync::Mutex::lock 可能占用/阻塞线程。
        // 此处把整个同步业务交给专用阻塞线程池，保持异步工作线程可调度其他请求。
        // Arc::clone 共享状态而不复制账号；此时 HTTP 层没有持有用户表的锁。
        let service = self.0.clone();
        // move 闭包取得 service/method/path/body/authorization 的所有权，跨线程使用。
        // spawn_blocking 立即提交工作并返回 JoinHandle；闭包内部仍是普通同步函数。
        let (status, body) = rocket::tokio::task::spawn_blocking(move || {
            service.handle(&method, &path, &body, &authorization)
        })
        // 等待阻塞线程的结果；未完成时暂停当前请求，让线程执行其他就绪任务。
        .await
        // 如果阻塞任务 panic 等导致 JoinError，转换为 HTTP 500。
        .unwrap_or_else(|_| error(500, "Handler failed"));
        // 同名 body 重新绑定为响应 JSON，最后交给 Rocket 发送给客户端。
        Outcome::from(request, (Status::new(status), Json(body)))
    }
}

/// 构造应用供 main 启动，也供 HTTP 测试直接调用。
pub fn create_app() -> Rocket<Build> {
    // 每次创建应用都创建独立的内存用户表；同一个应用内的处理器共享此表。
    let dispatch = Dispatch(Arc::new(Service::default()));
    // Vec<_> 表示向量，元素类型由编译器推断为 Route。
    // 先为多种方法注册框架入口，具体允许哪些方法由 ROUTES/route_error 决定。
    let routes: Vec<_> = [
        Method::Get,
        Method::Post,
        Method::Put,
        Method::Delete,
        Method::Patch,
        Method::Head,
        Method::Options,
        Method::Trace,
        Method::Connect,
    ]
    // 消费方法数组，将每个方法转换为一条框架路由。
    .into_iter()
    // /<_..> 是捕获剩余路径的通配路由；统一交给 Dispatch 判断业务路径。
    // clone 为各路由提供处理器，但它们仍指向同一个 Service。
    .map(|method| Route::new(method, "/<_..>", dispatch.clone()))
    // 把迭代器产生的路由收集到 Vec 中。
    .collect();
    // 创建 Rocket，安装日志钩子，并把路由挂到根路径 /；此处还没有启动监听。
    rocket::build().attach(ConsoleOutput).mount("/", routes)
}
