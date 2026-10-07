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
