//! 用本机临时 TCP 服务端检查客户端实际发出的 HTTP 请求和收到的错误响应。

use reqwest::{Method, blocking::Client};
use rm_client_sync::exchange;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

#[test]
fn sends_http_authorization_and_preserves_error_status() {
    // 端口 0 让系统分配空闲端口，测试不依赖预先运行的项目服务端。
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    // 服务端放在另一线程：主线程才能同时调用阻塞的 exchange。
    let peer = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        // 克隆 TCP 句柄供 BufReader 读取；原 stream 留着发送模拟响应。
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut headers = String::new();
        // HTTP 请求头以空行结束；这里只需读取头部，不读取请求体。
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        // 检查请求路径，以及 bearer_auth 生成的 Authorization 请求头。
        assert!(headers.starts_with("GET /texts HTTP/1.1\r\n"));
        assert!(
            headers
                .to_lowercase()
                .contains("authorization: bearer sample\r\n")
        );
        // 模拟纯文本的 401 响应，验证客户端不会因非 JSON 响应体丢失状态码。
        stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 7\r\nConnection: close\r\n\r\nexpired").unwrap();
    });
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let result = exchange(&client, &url, Method::GET, "/texts", "sample", None).unwrap();
    assert_eq!(result, (401, json!({"message":"expired"})));
    // 等待模拟服务端结束，也把该线程的断言失败传回测试主线程。
    peer.join().unwrap();
}
