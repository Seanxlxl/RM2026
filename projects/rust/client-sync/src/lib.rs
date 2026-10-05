//! 同步 HTTP 请求的公共部分：组装请求、发送请求，并把响应交给命令行入口处理。

use reqwest::{Method, blocking::Client};
use serde_json::Value;

/// 发送一次 HTTP 请求，返回 `(状态码, 响应内容)`。
///
/// `client` 是可复用的同步客户端；`body` 为 `None` 时不发送 JSON 请求体。
/// HTTP 的 4xx/5xx 仍是收到的响应，因此保留状态码放在 `Ok` 中；
/// 连接或读取响应失败才通过 `Err` 返回。
pub fn exchange(
    client: &Client,
    url: &str,
    method: Method,
    path: &str,
    token: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), reqwest::Error> {
    // 去掉基础地址末尾的斜杠，再拼接以 / 开头的 path，避免出现双斜杠。
    // request 是构建器；后面的设置会消费旧构建器并返回新的构建器。
    let mut request = client.request(method, format!("{}{path}", url.trim_end_matches('/')));
    if !token.is_empty() {
        // reqwest 会生成 Authorization: Bearer <token> 请求头。
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        // 把 serde_json::Value 序列化为 JSON，并设置相应的 Content-Type。
        request = request.json(body);
    }
    // send() 会阻塞当前线程直到收到响应或发生错误；? 把错误直接返回给调用者。
    let response = request.send()?;
    // 先保存状态码：接下来 text() 会消费 response，之后无法再读取其状态。
    let status = response.status().as_u16();
    let text = response.text()?;
    // 协议通常返回 JSON；若响应体不是 JSON（例如纯文本错误），仍保留其原文供显示。
    let value =
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({"message": text}));
    Ok((status, value))
}
