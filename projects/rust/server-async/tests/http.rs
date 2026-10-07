//! HTTP 集成测试：通过 Rocket 本地客户端走完整处理链，无需占用 TCP 端口。
//! blocking::Client 让测试使用同步写法；应用内部仍执行异步请求处理。

// 复用正式启动入口的应用组装，避免测试另一套实现。
use rm_server_async::http::create_app;
// ContentType/Header 用于构造请求头，Status 用于检查响应状态。
use rocket::http::{ContentType, Header, Status};
use rocket::local::blocking::Client;
use serde_json::{Value, json};

// 验证从注册、登录、列出文本到退出的完整 HTTP 账号流程。
#[test]
fn http_account_lifecycle() {
    // tracked 构造本地客户端（可跟踪 cookie）；本项目实际使用 Bearer 令牌。
    // unwrap 使应用构造失败时立即让测试失败。
    let client = Client::tracked(create_app()).unwrap();
    // get 构造请求，dispatch 发送给本地 Rocket 应用并等待响应。
    let ping = client.get("/ping").dispatch();
    assert_eq!(ping.status(), Status::Ok);
    // into_json 消费响应并解析 JSON；::<Value> 指定解析类型。
    assert_eq!(ping.into_json::<Value>().unwrap(), json!({"data": "pong"}));
    // 请求体要发字节/文本，所以把 JSON 值序列化为字符串。
    let account = json!({"username": "alice", "password": "password1"}).to_string();
    // header 设置 Content-Type；body 设置请求体；最后检查注册返回 201。
    assert_eq!(
        client
            .post("/users")
            .header(ContentType::JSON)
            .body(&account)
            .dispatch()
            .status(),
        Status::Created
    );
    // 用同样的账号登录，并读取响应里的 token。
    let login = client
        .post("/sessions")
        .header(ContentType::JSON)
        .body(&account)
        .dispatch()
        .into_json::<Value>()
        .unwrap();
    // JSON 索引取出令牌；as_str 确认类型，format! 添加协议要求的前缀。
    let authorization = format!("Bearer {}", login["data"]["token"].as_str().unwrap());
    // clone 令牌字符串供多个请求使用；Header::new 构造 Authorization 请求头。
    let texts = client
        .get("/texts")
        .header(Header::new("Authorization", authorization.clone()))
        .dispatch();
    assert_eq!(texts.status(), Status::Ok);
    // 新账号没有保存文本，列表应为空；它不是用户名列表。
    assert_eq!(texts.into_json::<Value>().unwrap(), json!({"data": []}));
    // 同一接口不带令牌时必须返回 401。
    assert_eq!(
        client.get("/texts").dispatch().status(),
        Status::Unauthorized
    );
    // 正确令牌可以退出；退出后该令牌再列出文本应返回 401。
    assert_eq!(
        client
            .delete("/sessions/current")
            .header(Header::new("Authorization", authorization.clone()))
            .dispatch()
            .status(),
        Status::Ok
    );
    assert_eq!(
        client
            .get("/texts")
            .header(Header::new("Authorization", authorization))
            .dispatch()
            .status(),
        Status::Unauthorized
    );
}

// 验证非法请求体、请求体大小边界，以及未知路径和错误方法的响应。
#[test]
fn http_input_and_routing() {
    let client = Client::tracked(create_app()).unwrap();
    // 三种非法请求体：普通文本、非法 UTF-8 字节、JSON 不允许的 NaN。
    for body in [b"not JSON".to_vec(), vec![0xff], b"NaN".to_vec()] {
        assert_eq!(
            client
                .post("/users")
                .header(ContentType::JSON)
                .body(body)
                .dispatch()
                .status(),
            Status::BadRequest
        );
    }
    // format! 中 {{ 和 }} 表示字面量花括号；{} 是后面字符串的占位符。
    // 空对象加空格使总长度恰好为上限；JSON 合法，但缺账号字段，故为 400。
    let exact = format!("{{}}{}", " ".repeat(524_288 - 2));
    assert_eq!(
        client
            .post("/users")
            .header(ContentType::JSON)
            .body(&exact)
            .dispatch()
            .status(),
        Status::BadRequest
    );
    assert_eq!(
        client
            .post("/users")
            .header(ContentType::JSON)
            // 在上限基础上再加一个字节，应在读取阶段返回 413。
            .body(format!("{exact} "))
            .dispatch()
            .status(),
        Status::PayloadTooLarge
    );
    assert_eq!(
        client
            .post("/users")
            .header(ContentType::JSON)
            // r#"..."# 是原始字符串，内部双引号不用转义；用户名是布尔值，非法。
            .body(r#"{"username":true,"password":"password1"}"#)
            .dispatch()
            .status(),
        Status::BadRequest
    );
    // 未知路径为 404；已知路径用错误方法为 405。
    assert_eq!(client.get("/missing").dispatch().status(), Status::NotFound);
    assert_eq!(client.get("/echo").dispatch().status(), Status::NotFound);
    assert_eq!(
        client.patch("/ping").dispatch().status(),
        Status::MethodNotAllowed
    );
}

// 验证起始服务端的待实现路由为 404，已知路径的不支持方法为 405。
#[test]
fn unimplemented_routes_are_absent() {
    use rocket::http::Method;
    let client = Client::tracked(create_app()).unwrap();
    // 这是起始状态测试：这些待实现接口现在返回 404。
    // 以后完成接口，要把此测试更新为验证新行为，而不是继续期待 404。
    for (method, path) in [
        (Method::Post, "/echo"),
        (Method::Delete, "/users/me"),
        (Method::Put, "/texts/note"),
        (Method::Get, "/texts/note"),
        (Method::Delete, "/texts/note"),
    ] {
        // req 允许动态指定 HTTP 方法，不限于 get/post 等快捷方法。
        assert_eq!(
            client.req(method, path).dispatch().status(),
            Status::NotFound
        );
    }
    // 这五个路径已实现，但都不支持 PATCH，因此应返回 405。
    for path in [
        "/ping",
        "/users",
        "/sessions",
        "/sessions/current",
        "/texts",
    ] {
        assert_eq!(
            client.patch(path).dispatch().status(),
            Status::MethodNotAllowed
        );
    }
}
