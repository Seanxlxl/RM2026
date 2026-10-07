//! 程序入口：解析监听地址 → 组装服务端 → 启动并等待退出。
//! 阅读顺序建议：本文件 → http.rs（请求怎么处理）→ lib.rs（业务怎么处理）。

// 引入 trait 后，Args 才能调用 clap 提供的 parse()。
use clap::Parser;
// 包名 rm-server-async 在 Rust 的 use 路径中写成 rm_server_async。
use rm_server_async::http::create_app;
// SocketAddr 同时保存 IP 地址和端口，解析时会检查地址是否合法。
use std::net::SocketAddr;

// derive 让 clap 根据结构体自动生成命令行解析代码。
#[derive(Parser)]
struct Args {
    // long 生成 --address 参数；默认只监听本机的 7878 端口。
    #[arg(long, default_value = "127.0.0.1:7878")]
    address: SocketAddr,
}

// 解析命令行参数并启动 HTTP 服务，等待服务关闭或返回启动错误。
// 普通 main 不能直接 await；这个宏负责建立 Rocket 使用的异步运行时。
#[rocket::main]
// async fn 的调用产生 Future（表示待完成计算的值）；由运行时推进这个入口。
// Result 表示成功或失败；Box<dyn Error> 可以容纳不同类型的错误。
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 从命令行读取参数；非法参数由 clap 报错并退出。
    let args = Args::parse();
    // 此时只是创建应用和内存状态，尚未开始监听网络。
    let app = create_app();
    // Figment 是 Rocket 的配置对象；clone 复制配置，merge 覆盖指定配置项。
    let config = app
        .figment()
        .clone()
        // 拆出 SocketAddr 的 IP 和端口，分别交给 Rocket。
        .merge(("address", args.address.ip()))
        .merge(("port", args.address.port()))
        // 减少框架日志；http.rs 中自定义的请求日志仍会输出。
        .merge(("log_level", "critical"));
    // configure 应用配置；launch 开始监听、接收请求，并等待服务停止。
    // .await 在需要等待时允许运行时处理其他任务；? 将启动等错误向外返回。
    app.configure(config).launch().await?;
    // 正常关闭后返回成功；() 是“没有额外返回数据”的单位类型。
    Ok(())
}
