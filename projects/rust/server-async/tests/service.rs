//! 业务集成测试：直接调用 Service，不启动 HTTP 服务，也不需要真实网络。

use rm_server_async::Service;
// Value::Null 表示无请求体，json! 构造测试输入及期望响应。
use serde_json::{Value, json};

// cargo test 自动运行带 #[test] 的函数；断言不满足时测试失败。
// 验证公开 ping、账号输入校验、未登录鉴权失败和未知路径。
#[test]
fn input_validation_and_baseline() {
    // 每个测试使用独立状态；default() 创建空用户表。
    let service = Service::default();
    // 检查完整返回元组（状态码，JSON），不只是检查状态码。
    assert_eq!(
        service.handle("GET", "/ping", &Value::Null, ""),
        (200, json!({"data":"pong"}))
    );
    // 依次测试：null、数组、用户名类型错误、用户名含非法字符。
    for body in [
        Value::Null,
        json!([]),
        json!({"username":true,"password":"password1"}),
        json!({"username":"a/b","password":"password1"}),
    ] {
        // .0 只取返回元组的状态码；这些输入都应被拒绝。
        assert_eq!(service.handle("POST", "/users", &body, "").0, 400);
    }
    // 未登录不能列出文本；未知路径应为 404。
    assert_eq!(service.handle("GET", "/texts", &Value::Null, "").0, 401);
    assert_eq!(service.handle("GET", "/missing", &Value::Null, "").0, 404);
}

// 用四个线程注册同一用户名，验证查重和插入的锁保护只允许一次成功。
#[test]
fn concurrent_registration_has_one_winner() {
    // 多个线程通过 Arc 共享同一个 Service，Service 内部的 Mutex 保护用户表。
    let service = std::sync::Arc::new(Service::default());
    // 0..4 产生四个编号；_ 表示不使用编号，只需要启动四份工作。
    let workers: Vec<_> = (0..4)
        .map(|_| {
            // 复制共享指针，再用 move 将这一份交给新线程。
            let service = service.clone();
            std::thread::spawn(move || {
                // 四个线程注册同一个名字；map 阶段就启动，不等前一个完成。
                service
                    .handle(
                        "POST",
                        "/users",
                        &json!({"username":"alice","password":"password1"}),
                        "",
                    )
                    .0
            })
        })
        // 收集线程句柄，此时四个线程都已启动。
        .collect();
    // 消费句柄向量；join 等待每个线程结束，取得该请求的状态码。
    let statuses: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    // &&s 解引用 filter 收到的双重引用；count 统计符合条件的状态码数量。
    // 只能一次创建成功，另外三次必须报重复，验证查重与插入的锁保护。
    assert_eq!(statuses.iter().filter(|&&s| s == 201).count(), 1);
    assert_eq!(statuses.iter().filter(|&&s| s == 409).count(), 3);
}

/// 为文本业务测试注册并登录用户，返回完整的 Authorization 值。
fn register_and_login(service: &Service, username: &str) -> String {
    let account = json!({"username": username, "password": "password1"});
    assert_eq!(service.handle("POST", "/users", &account, "").0, 201);
    let (status, login) = service.handle("POST", "/sessions", &account, "");
    assert_eq!(status, 200);
    format!("Bearer {}", login["data"]["token"].as_str().unwrap())
}

/// 注销撤销旧身份和文本，同名重注册保持空状态且不影响其他用户。
#[test]
fn account_deletion_revokes_identity_and_isolates_reregistration() {
    let service = Service::default();
    let old = register_and_login(&service, "alice");
    let bob = register_and_login(&service, "bob");
    for authorization in [&old, &bob] {
        assert_eq!(
            service
                .handle(
                    "PUT",
                    "/texts/note",
                    &json!({"text": "原文"}),
                    authorization
                )
                .0,
            200
        );
    }
    for authorization in ["", "Bearer invalid"] {
        assert_eq!(
            service
                .handle("DELETE", "/users/me", &Value::Null, authorization)
                .0,
            401
        );
    }
    assert_eq!(
        service.handle("DELETE", "/users/me", &Value::Null, &old),
        (200, json!({"data": null}))
    );
    assert!(!service.users.lock().unwrap().contains_key("alice"));
    for (method, path, body) in [
        ("GET", "/texts", Value::Null),
        ("GET", "/texts/note", Value::Null),
        ("PUT", "/texts/note", json!({"text": "旧身份不能写入"})),
        ("DELETE", "/texts/note", Value::Null),
        ("DELETE", "/sessions/current", Value::Null),
        ("DELETE", "/users/me", Value::Null),
    ] {
        assert_eq!(service.handle(method, path, &body, &old).0, 401);
    }
    // 使用完全相同的用户名和密码，也不能继承旧数据或恢复旧令牌。
    let current = register_and_login(&service, "alice");
    assert_eq!(
        service.handle("DELETE", "/users/me", &Value::Null, &old).0,
        401
    );
    assert_eq!(
        service.handle("GET", "/texts", &Value::Null, &current),
        (200, json!({"data": []}))
    );
    assert_eq!(
        service
            .handle("GET", "/texts/note", &Value::Null, &current)
            .0,
        404
    );
    assert_eq!(
        service.handle("GET", "/texts/note", &Value::Null, &bob),
        (200, json!({"data": "原文"}))
    );
}

/// 上传和注销竞争时，最终旧账号消失，同名新账号没有旧请求写入的文本。
#[test]
fn concurrent_upload_and_deletion_leave_no_old_data() {
    use std::sync::{Arc, Barrier};

    let service = Arc::new(Service::default());
    let old = register_and_login(&service, "alice");
    let start = Arc::new(Barrier::new(3));
    let upload = {
        let service = Arc::clone(&service);
        let start = Arc::clone(&start);
        let old = old.clone();
        std::thread::spawn(move || {
            start.wait();
            service
                .handle("PUT", "/texts/note", &json!({"text": "并发正文"}), &old)
                .0
        })
    };
    let deletion = {
        let service = Arc::clone(&service);
        let start = Arc::clone(&start);
        let old = old.clone();
        std::thread::spawn(move || {
            start.wait();
            service.handle("DELETE", "/users/me", &Value::Null, &old)
        })
    };
    start.wait();
    let upload_status = upload.join().unwrap();
    assert!(matches!(upload_status, 200 | 401));
    assert_eq!(deletion.join().unwrap(), (200, json!({"data": null})));
    assert!(!service.users.lock().unwrap().contains_key("alice"));
    let current = register_and_login(&service, "alice");
    assert_eq!(
        service
            .handle("PUT", "/texts/note", &json!({"text": "迟到的旧请求"}), &old)
            .0,
        401
    );
    assert_eq!(
        service.handle("GET", "/texts", &Value::Null, &current),
        (200, json!({"data": []}))
    );
}

/// 回显必须保留空正文、Unicode、空白和换行，并按 UTF-8 字节限制大小。
#[test]
fn echo_preserves_text_and_checks_utf8_limit() {
    let service = Service::default();
    for text in ["", "你好 Rust 😀", "  第一行\r\n第二行\n  ", "\0\t"] {
        assert_eq!(
            service.handle("POST", "/echo", &json!({"text": text}), ""),
            (200, json!({"data": text}))
        );
    }
    // 同样达到字节上限的 ASCII 和 emoji，字符数量并不相同。
    for text in ["a".repeat(65_536), "😀".repeat(16_384)] {
        assert_eq!(
            service.handle("POST", "/echo", &json!({"text": text}), ""),
            (200, json!({"data": text}))
        );
        assert_eq!(
            service
                .handle("POST", "/echo", &json!({"text": format!("{text}a")}), "")
                .0,
            413
        );
    }
}

/// 回显请求必须恰好包含一个字符串 text 字段，类型不能自动转换。
#[test]
fn echo_rejects_invalid_fields() {
    let service = Service::default();
    for body in [
        Value::Null,
        json!([]),
        json!("hello"),
        json!({}),
        json!({"other": "hello"}),
        json!({"text": null}),
        json!({"text": 42}),
        json!({"text": true}),
        json!({"text": []}),
        json!({"text": {}}),
        json!({"text": "hello", "extra": true}),
    ] {
        assert_eq!(service.handle("POST", "/echo", &body, "").0, 400, "{body}");
    }
}

/// 上传后应能原样读取；同名上传覆盖，大小写不同的名称分别保存。
#[test]
fn texts_round_trip_overwrite_and_list() {
    let service = Service::default();
    let authorization = register_and_login(&service, "alice");
    for text in ["你好\nRust 😀\n", "  覆盖后的正文  ", ""] {
        assert_eq!(
            service.handle("PUT", "/texts/note", &json!({"text": text}), &authorization),
            (200, json!({"data": null}))
        );
        assert_eq!(
            service.handle("GET", "/texts/note", &Value::Null, &authorization),
            (200, json!({"data": text}))
        );
    }
    for name in ["zeta", "Note", "alpha"] {
        assert_eq!(
            service
                .handle(
                    "PUT",
                    &format!("/texts/{name}"),
                    &json!({"text": name}),
                    &authorization
                )
                .0,
            200
        );
    }
    assert_eq!(
        service.handle("GET", "/texts", &Value::Null, &authorization),
        (200, json!({"data": ["Note", "alpha", "note", "zeta"]}))
    );
    assert_eq!(
        service.handle("GET", "/texts/Note", &Value::Null, &authorization),
        (200, json!({"data": "Note"}))
    );
    assert_eq!(
        service
            .handle("GET", "/texts/missing", &Value::Null, &authorization)
            .0,
        404
    );
}

/// 上传字段或正文大小不合法时，不能覆盖之前保存的内容。
#[test]
fn invalid_uploads_preserve_existing_text() {
    let service = Service::default();
    let authorization = register_and_login(&service, "alice");
    assert_eq!(
        service
            .handle(
                "PUT",
                "/texts/note",
                &json!({"text": "原文"}),
                &authorization
            )
            .0,
        200
    );
    for body in [
        Value::Null,
        json!([]),
        json!("hello"),
        json!({}),
        json!({"other": "hello"}),
        json!({"text": null}),
        json!({"text": 42}),
        json!({"text": true}),
        json!({"text": []}),
        json!({"text": {}}),
        json!({"text": "hello", "extra": true}),
    ] {
        assert_eq!(
            service
                .handle("PUT", "/texts/note", &body, &authorization)
                .0,
            400,
            "{body}"
        );
    }
    assert_eq!(
        service
            .handle(
                "PUT",
                "/texts/note",
                &json!({"text": "😀".repeat(16_385)}),
                &authorization
            )
            .0,
        413
    );
    assert_eq!(
        service.handle("GET", "/texts/note", &Value::Null, &authorization),
        (200, json!({"data": "原文"}))
    );
}

/// 名称和正文接受恰好达到上限的值，超过上限或含非法字符时拒绝。
#[test]
fn text_name_and_utf8_boundaries() {
    let service = Service::default();
    let authorization = register_and_login(&service, "alice");
    for name in ["a".to_owned(), "Ab_09-".to_owned(), "a".repeat(64)] {
        let path = format!("/texts/{name}");
        for text in ["a".repeat(65_536), "😀".repeat(16_384)] {
            assert_eq!(
                service
                    .handle("PUT", &path, &json!({"text": text}), &authorization)
                    .0,
                200
            );
            assert_eq!(
                service.handle("GET", &path, &Value::Null, &authorization),
                (200, json!({"data": text}))
            );
            assert_eq!(
                service
                    .handle(
                        "PUT",
                        &path,
                        &json!({"text": format!("{text}a")}),
                        &authorization
                    )
                    .0,
                413
            );
        }
    }
    for name in [
        "".to_owned(),
        "a".repeat(65),
        "中文".to_owned(),
        "bad.name".to_owned(),
        "bad name".to_owned(),
    ] {
        let path = format!("/texts/{name}");
        assert_eq!(
            service
                .handle("PUT", &path, &json!({"text": "hello"}), &authorization)
                .0,
            400,
            "{path}"
        );
        assert_eq!(
            service.handle("GET", &path, &Value::Null, &authorization).0,
            400,
            "{path}"
        );
    }
}

/// 每个用户的文本独立保存，其他用户的独有名称不能被读取或列出。
#[test]
fn texts_are_isolated_between_users() {
    let service = Service::default();
    let alice = register_and_login(&service, "alice");
    let bob = register_and_login(&service, "bob");
    for (authorization, text) in [(&alice, "Alice 的笔记"), (&bob, "Bob 的笔记")] {
        assert_eq!(
            service
                .handle("PUT", "/texts/note", &json!({"text": text}), authorization)
                .0,
            200
        );
    }
    assert_eq!(
        service
            .handle(
                "PUT",
                "/texts/alice-only",
                &json!({"text": "私有文本"}),
                &alice
            )
            .0,
        200
    );
    for (authorization, text) in [(&alice, "Alice 的笔记"), (&bob, "Bob 的笔记")] {
        assert_eq!(
            service.handle("GET", "/texts/note", &Value::Null, authorization),
            (200, json!({"data": text}))
        );
    }
    assert_eq!(
        service
            .handle("GET", "/texts/alice-only", &Value::Null, &bob)
            .0,
        404
    );
    assert_eq!(
        service.handle("GET", "/texts", &Value::Null, &bob),
        (200, json!({"data": ["note"]}))
    );
    assert_eq!(
        service.handle("GET", "/texts", &Value::Null, &alice),
        (200, json!({"data": ["alice-only", "note"]}))
    );
}

/// 缺失、伪造、被替换和退出后的令牌均不能读写，失败写入不能改变正文。
#[test]
fn text_access_requires_current_token() {
    let service = Service::default();
    let old = register_and_login(&service, "alice");
    assert_eq!(
        service
            .handle("PUT", "/texts/note", &json!({"text": "原文"}), &old)
            .0,
        200
    );
    let account = json!({"username": "alice", "password": "password1"});
    let (status, login) = service.handle("POST", "/sessions", &account, "");
    assert_eq!(status, 200);
    let current = format!("Bearer {}", login["data"]["token"].as_str().unwrap());
    for authorization in [
        "",
        "Bearer ",
        "Bearer invalid",
        "Basic invalid",
        old.as_str(),
    ] {
        assert_eq!(
            service
                .handle(
                    "PUT",
                    "/texts/note",
                    &json!({"text": "不应保存"}),
                    authorization
                )
                .0,
            401
        );
        assert_eq!(
            service
                .handle("GET", "/texts/note", &Value::Null, authorization)
                .0,
            401
        );
    }
    assert_eq!(
        service.handle("GET", "/texts/note", &Value::Null, &current),
        (200, json!({"data": "原文"}))
    );
    assert_eq!(
        service
            .handle("DELETE", "/sessions/current", &Value::Null, &current)
            .0,
        200
    );
    assert_eq!(
        service
            .handle("PUT", "/texts/note", &json!({"text": "不应保存"}), &current)
            .0,
        401
    );
    assert_eq!(
        service
            .handle("GET", "/texts/note", &Value::Null, &current)
            .0,
        401
    );
}
