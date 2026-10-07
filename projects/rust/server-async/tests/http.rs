//! HTTP 集成测试：通过 Rocket 本地客户端走完整处理链，无需占用 TCP 端口。
//! blocking::Client 让测试使用同步写法；应用内部仍执行异步请求处理。

// 复用正式启动入口的应用组装，避免测试另一套实现。
use rm_server_async::http::create_app;
// ContentType/Header 用于构造请求头，Status 用于检查响应状态。
use rocket::http::{ContentType, Header, Method, Status};
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
    // /echo 已存在，但不支持 GET，因此应返回 405。
    assert_eq!(
        client.get("/echo").dispatch().status(),
        Status::MethodNotAllowed
    );
    // /ping 已存在，但不支持 PATCH，因此应返回 405。
    assert_eq!(
        client.patch("/ping").dispatch().status(),
        Status::MethodNotAllowed
    );
}

/// 验证受保护删除接口要求登录，以及已知路径上不支持的方法。
#[test]
fn remaining_routes_and_unsupported_methods() {
    let client = Client::tracked(create_app()).unwrap();
    assert_eq!(
        client.delete("/users/me").dispatch().status(),
        Status::Unauthorized
    );
    // 删除文本已实现，但缺少令牌时不能访问。
    assert_eq!(
        client.delete("/texts/note").dispatch().status(),
        Status::Unauthorized
    );
    for path in [
        "/ping",
        "/users",
        "/sessions",
        "/sessions/current",
        "/users/me",
        "/texts",
        "/echo",
        "/texts/note",
    ] {
        assert_eq!(
            client.patch(path).dispatch().status(),
            Status::MethodNotAllowed
        );
    }
}

/// 为 HTTP 文本测试注册并登录用户，返回可复用的请求头值。
fn register_and_login(client: &Client, username: &str) -> String {
    let account = json!({"username": username, "password": "password1"}).to_string();
    assert_eq!(
        client
            .post("/users")
            .header(ContentType::JSON)
            .body(&account)
            .dispatch()
            .status(),
        Status::Created
    );
    let login = client
        .post("/sessions")
        .header(ContentType::JSON)
        .body(&account)
        .dispatch();
    assert_eq!(login.status(), Status::Ok);
    let login = login.into_json::<Value>().unwrap();
    format!("Bearer {}", login["data"]["token"].as_str().unwrap())
}

/// 从 HTTP 入口验证注销响应、旧令牌撤销和同名重注册后的空文本列表。
#[test]
fn http_account_deletion_and_reregistration() {
    let client = Client::tracked(create_app()).unwrap();
    let old = register_and_login(&client, "alice");
    assert_eq!(
        client
            .put("/texts/note")
            .header(ContentType::JSON)
            .header(Header::new("Authorization", old.clone()))
            .body(json!({"text": "旧账号正文"}).to_string())
            .dispatch()
            .status(),
        Status::Ok
    );
    let deletion = client
        .delete("/users/me")
        .header(Header::new("Authorization", old.clone()))
        .dispatch();
    assert_eq!(deletion.status(), Status::Ok);
    assert_eq!(deletion.content_type(), Some(ContentType::JSON));
    assert_eq!(
        deletion.into_json::<Value>().unwrap(),
        json!({"data": null})
    );
    for (method, path) in [(Method::Get, "/texts"), (Method::Delete, "/users/me")] {
        assert_eq!(
            client
                .req(method, path)
                .header(Header::new("Authorization", old.clone()))
                .dispatch()
                .status(),
            Status::Unauthorized
        );
    }
    let current = register_and_login(&client, "alice");
    let list = client
        .get("/texts")
        .header(Header::new("Authorization", current.clone()))
        .dispatch();
    assert_eq!(list.status(), Status::Ok);
    assert_eq!(list.into_json::<Value>().unwrap(), json!({"data": []}));
    assert_eq!(
        client
            .get("/texts/note")
            .header(Header::new("Authorization", current))
            .dispatch()
            .status(),
        Status::NotFound
    );
    assert_eq!(
        client
            .delete("/users/me")
            .header(Header::new("Authorization", old))
            .dispatch()
            .status(),
        Status::Unauthorized
    );
}

/// 走完整 HTTP 处理链验证公开回显、Unicode 和 JSON 响应封装。
#[test]
fn http_echo_round_trip_and_utf8_limit() {
    let client = Client::tracked(create_app()).unwrap();
    for text in [
        "".to_owned(),
        "  你好\r\nRust 😀\n  ".to_owned(),
        "\0\t".to_owned(),
        "a".repeat(65_536),
        "😀".repeat(16_384),
    ] {
        let response = client
            .post("/echo")
            .header(ContentType::JSON)
            .body(json!({"text": text}).to_string())
            .dispatch();
        assert_eq!(response.status(), Status::Ok);
        assert_eq!(response.content_type(), Some(ContentType::JSON));
        assert_eq!(
            response.into_json::<Value>().unwrap(),
            json!({"data": text})
        );
    }
    for text in ["a".repeat(65_537), format!("{}a", "😀".repeat(16_384))] {
        assert_eq!(
            client
                .post("/echo")
                .header(ContentType::JSON)
                .body(json!({"text": text}).to_string())
                .dispatch()
                .status(),
            Status::PayloadTooLarge
        );
    }
}

/// echo 和 PUT 都应拒绝非法 JSON、非法 UTF-8 和不符合协议的字段。
#[test]
fn http_text_endpoints_reject_invalid_bodies() {
    let client = Client::tracked(create_app()).unwrap();
    let authorization = register_and_login(&client, "alice");
    for (method, path) in [(Method::Post, "/echo"), (Method::Put, "/texts/note")] {
        for body in [
            b"not JSON".to_vec(),
            vec![0xff],
            br#"{"text":"\uD800"}"#.to_vec(),
            b"null".to_vec(),
            b"[]".to_vec(),
            br#""hello""#.to_vec(),
            b"{}".to_vec(),
            br#"{"other":"hello"}"#.to_vec(),
            br#"{"text":null}"#.to_vec(),
            br#"{"text":42}"#.to_vec(),
            br#"{"text":true}"#.to_vec(),
            br#"{"text":[]}"#.to_vec(),
            br#"{"text":{}}"#.to_vec(),
            br#"{"text":"hello","extra":true}"#.to_vec(),
        ] {
            assert_eq!(
                client
                    .req(method, path)
                    .header(ContentType::JSON)
                    .header(Header::new("Authorization", authorization.clone()))
                    .body(body)
                    .dispatch()
                    .status(),
                Status::BadRequest,
                "{method} {path}"
            );
        }
        // 单个错误请求不应影响后续合法请求。
        assert_eq!(
            client
                .req(method, path)
                .header(ContentType::JSON)
                .header(Header::new("Authorization", authorization.clone()))
                .body(r#"{"text":"恢复正常"}"#)
                .dispatch()
                .status(),
            Status::Ok
        );
    }
}

/// 请求体上限按完整 JSON 字节计算，恰好达到上限接受，超出一个字节拒绝。
#[test]
fn http_text_request_body_limit() {
    let client = Client::tracked(create_app()).unwrap();
    let authorization = register_and_login(&client, "alice");
    let base = r#"{"text":"保留正文"}"#;
    let exact = format!("{base}{}", " ".repeat(524_288 - base.len()));
    for (method, path) in [(Method::Post, "/echo"), (Method::Put, "/texts/note")] {
        let response = client
            .req(method, path)
            .header(ContentType::JSON)
            .header(Header::new("Authorization", authorization.clone()))
            .body(&exact)
            .dispatch();
        assert_eq!(response.status(), Status::Ok);
        let expected = if method == Method::Post {
            json!({"data": "保留正文"})
        } else {
            json!({"data": null})
        };
        assert_eq!(response.into_json::<Value>().unwrap(), expected);
        assert_eq!(
            client
                .req(method, path)
                .header(ContentType::JSON)
                .header(Header::new("Authorization", authorization.clone()))
                .body(format!("{exact} "))
                .dispatch()
                .status(),
            Status::PayloadTooLarge
        );
    }
    // 超限上传没有清空或截断之前保存的正文。
    let read = client
        .get("/texts/note")
        .header(Header::new("Authorization", authorization))
        .dispatch();
    assert_eq!(read.status(), Status::Ok);
    assert_eq!(
        read.into_json::<Value>().unwrap(),
        json!({"data": "保留正文"})
    );
}

/// 上传和覆盖后均能读回原文，并且列表不会因覆盖出现重复名称。
#[test]
fn http_text_upload_read_and_overwrite() {
    let client = Client::tracked(create_app()).unwrap();
    let authorization = register_and_login(&client, "alice");
    for text in ["你好\nRust 😀\n", "  覆盖后的正文  ", ""] {
        let upload = client
            .put("/texts/note")
            .header(ContentType::JSON)
            .header(Header::new("Authorization", authorization.clone()))
            .body(json!({"text": text}).to_string())
            .dispatch();
        assert_eq!(upload.status(), Status::Ok);
        assert_eq!(upload.into_json::<Value>().unwrap(), json!({"data": null}));
        let read = client
            .get("/texts/note")
            .header(Header::new("Authorization", authorization.clone()))
            .dispatch();
        assert_eq!(read.status(), Status::Ok);
        assert_eq!(read.content_type(), Some(ContentType::JSON));
        assert_eq!(read.into_json::<Value>().unwrap(), json!({"data": text}));
    }
    let list = client
        .get("/texts")
        .header(Header::new("Authorization", authorization.clone()))
        .dispatch();
    assert_eq!(list.status(), Status::Ok);
    assert_eq!(
        list.into_json::<Value>().unwrap(),
        json!({"data": ["note"]})
    );
    assert_eq!(
        client
            .get("/texts/missing")
            .header(Header::new("Authorization", authorization))
            .dispatch()
            .status(),
        Status::NotFound
    );
    assert_eq!(
        client.get("/texts/a/b").dispatch().status(),
        Status::NotFound
    );
}

/// 验证 HTTP 名称长度和正文 UTF-8 边界，失败上传不能修改已有文本。
#[test]
fn http_text_name_and_content_boundaries() {
    let client = Client::tracked(create_app()).unwrap();
    let authorization = register_and_login(&client, "alice");
    let path = format!("/texts/{}", "a".repeat(64));
    for text in ["a".repeat(65_536), "😀".repeat(16_384)] {
        assert_eq!(
            client
                .put(&path)
                .header(ContentType::JSON)
                .header(Header::new("Authorization", authorization.clone()))
                .body(json!({"text": text}).to_string())
                .dispatch()
                .status(),
            Status::Ok
        );
        assert_eq!(
            client
                .put(&path)
                .header(ContentType::JSON)
                .header(Header::new("Authorization", authorization.clone()))
                .body(json!({"text": format!("{text}a")}).to_string())
                .dispatch()
                .status(),
            Status::PayloadTooLarge
        );
        let read = client
            .get(&path)
            .header(Header::new("Authorization", authorization.clone()))
            .dispatch();
        assert_eq!(read.status(), Status::Ok);
        assert_eq!(read.into_json::<Value>().unwrap(), json!({"data": text}));
    }
    for name in ["a".repeat(65), "bad.name".to_owned()] {
        let path = format!("/texts/{name}");
        assert_eq!(
            client
                .put(&path)
                .header(ContentType::JSON)
                .header(Header::new("Authorization", authorization.clone()))
                .body(r#"{"text":"hello"}"#)
                .dispatch()
                .status(),
            Status::BadRequest
        );
        assert_eq!(
            client
                .get(&path)
                .header(Header::new("Authorization", authorization.clone()))
                .dispatch()
                .status(),
            Status::BadRequest
        );
    }
}

/// 两名用户可保存同名文本，其他用户的独有名称既不能读取也不会出现在列表。
#[test]
fn http_texts_are_isolated_between_users() {
    let client = Client::tracked(create_app()).unwrap();
    let alice = register_and_login(&client, "alice");
    let bob = register_and_login(&client, "bob");
    for (authorization, text) in [(&alice, "Alice 的正文"), (&bob, "Bob 的正文")] {
        assert_eq!(
            client
                .put("/texts/note")
                .header(ContentType::JSON)
                .header(Header::new("Authorization", authorization.clone()))
                .body(json!({"text": text}).to_string())
                .dispatch()
                .status(),
            Status::Ok
        );
    }
    assert_eq!(
        client
            .put("/texts/alice-only")
            .header(ContentType::JSON)
            .header(Header::new("Authorization", alice.clone()))
            .body(r#"{"text":"private"}"#)
            .dispatch()
            .status(),
        Status::Ok
    );
    for (authorization, text) in [(&alice, "Alice 的正文"), (&bob, "Bob 的正文")] {
        let read = client
            .get("/texts/note")
            .header(Header::new("Authorization", authorization.clone()))
            .dispatch();
        assert_eq!(read.status(), Status::Ok);
        assert_eq!(read.into_json::<Value>().unwrap(), json!({"data": text}));
    }
    assert_eq!(
        client
            .get("/texts/alice-only")
            .header(Header::new("Authorization", bob.clone()))
            .dispatch()
            .status(),
        Status::NotFound
    );
    let list = client
        .get("/texts")
        .header(Header::new("Authorization", bob))
        .dispatch();
    assert_eq!(list.status(), Status::Ok);
    assert_eq!(
        list.into_json::<Value>().unwrap(),
        json!({"data": ["note"]})
    );
}

/// 缺失、伪造、重新登录前和退出后的令牌都必须被 HTTP 文本接口拒绝。
#[test]
fn http_text_access_requires_current_token() {
    let client = Client::tracked(create_app()).unwrap();
    let old = register_and_login(&client, "alice");
    assert_eq!(
        client
            .put("/texts/note")
            .header(ContentType::JSON)
            .header(Header::new("Authorization", old.clone()))
            .body(r#"{"text":"original"}"#)
            .dispatch()
            .status(),
        Status::Ok
    );
    let login = client
        .post("/sessions")
        .header(ContentType::JSON)
        .body(r#"{"username":"alice","password":"password1"}"#)
        .dispatch();
    assert_eq!(login.status(), Status::Ok);
    let login = login.into_json::<Value>().unwrap();
    let current = format!("Bearer {}", login["data"]["token"].as_str().unwrap());
    for authorization in [
        None,
        Some("Bearer "),
        Some("Bearer invalid"),
        Some("Basic invalid"),
        Some(old.as_str()),
    ] {
        for method in [Method::Put, Method::Get] {
            let mut request = client.req(method, "/texts/note");
            if let Some(authorization) = authorization {
                request = request.header(Header::new("Authorization", authorization.to_owned()));
            }
            if method == Method::Put {
                request = request
                    .header(ContentType::JSON)
                    .body(r#"{"text":"must not overwrite"}"#);
            }
            assert_eq!(request.dispatch().status(), Status::Unauthorized);
        }
    }
    let read = client
        .get("/texts/note")
        .header(Header::new("Authorization", current.clone()))
        .dispatch();
    assert_eq!(read.status(), Status::Ok);
    assert_eq!(
        read.into_json::<Value>().unwrap(),
        json!({"data": "original"})
    );
    assert_eq!(
        client
            .delete("/sessions/current")
            .header(Header::new("Authorization", current.clone()))
            .dispatch()
            .status(),
        Status::Ok
    );
    assert_eq!(
        client
            .put("/texts/note")
            .header(ContentType::JSON)
            .header(Header::new("Authorization", current.clone()))
            .body(r#"{"text":"must not overwrite"}"#)
            .dispatch()
            .status(),
        Status::Unauthorized
    );
    assert_eq!(
        client
            .get("/texts/note")
            .header(Header::new("Authorization", current))
            .dispatch()
            .status(),
        Status::Unauthorized
    );
}
