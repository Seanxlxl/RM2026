//! 同步业务层：根据方法、路径、JSON 和令牌，返回（HTTP 状态码，JSON）。
//! 这里不直接读网络，也没有 await；http.rs 在阻塞线程中调用 handle。
//! 数据只存于内存，重启就丢失；这是待你扩展的起始实现。

// pub mod 把 http.rs 声明为公开模块，让 main 和集成测试能访问它。
pub mod http;

// PBKDF2 配合 SHA-256 计算密码摘要；不保存明文密码。
use pbkdf2::pbkdf2_hmac;
// OsRng 从操作系统获取随机数；RngCore 提供 fill_bytes 方法。
use rand::{RngCore, rngs::OsRng};
// Value 能表示任意 JSON 值；json! 宏根据 Rust 数据构造 JSON。
use serde_json::{Value, json};
use sha2::Sha256;
// 有序键值表：遍历键时按键升序排列。
use std::collections::BTreeMap;
// Mutex（互斥锁）保护多个线程共享的数据，同一时刻只允许一个持锁者访问。
use std::sync::Mutex;
// 提供 ct_eq，避免普通相等比较因提前结束而泄露摘要匹配的进度。
use subtle::ConstantTimeEq;

// &[...] 是借用的切片；每项元组分别是 HTTP 方法和路径。
// 当前只有这五条业务路由。客户端显示命令，不代表服务端已经实现了它。
pub const ROUTES: &[(&str, &str)] = &[
    ("GET", "/ping"),
    ("POST", "/users"),
    ("POST", "/sessions"),
    ("DELETE", "/sessions/current"),
    ("GET", "/texts"),
];

/// None 表示方法和路径匹配；Some(状态码) 表示路由错误。
pub fn route_error(method: &str, path: &str) -> Option<u16> {
    // iter 遍历而不取走数据；find 查找路径匹配的第一项，_ 忽略方法字段。
    match ROUTES.iter().find(|(_, route)| *route == path) {
        // 没有已知路径：404。
        None => Some(404),
        // 找到了路径，但方法不同：405。if 是这个匹配分支的额外条件。
        Some((allowed, _)) if *allowed != method => Some(405),
        // 找到了路径，而且方法正确：继续处理。
        Some(_) => None,
    }
}

/// 一名用户的状态；用户名保存在 Service.users 的键中。
pub struct User {
    // 每个账号独立的随机盐，让相同密码也能得到不同的摘要。
    pub salt: [u8; 16],
    // 固定 32 字节的密码摘要，用于之后验证密码。
    pub digest: [u8; 32],
    // None = 未登录；Some(String) = 当前令牌。再次登录会替换它。
    pub token: Option<String>,
    // 文本名称 → 正文，不是用户名 → 用户；每名用户有自己的文本表。
    pub texts: BTreeMap<String, String>,
}

// 自动生成 default()：创建包在 Mutex 里的空用户表。
#[derive(Default)]
pub struct Service {
    // 用户名 → 用户状态。即使 handle 只借用 &self，也可通过锁安全地修改表。
    pub users: Mutex<BTreeMap<String, User>>,
}

/// 统一构造失败结果；u16 是状态码，Value 是 JSON 响应体。
pub fn error(status: u16, message: &str) -> (u16, Value) {
    (status, json!({"message": message}))
}

/// 名称必须非空、不超长，而且只能含 ASCII 字母、数字、下划线或连字符。
pub fn valid_name(name: &str, max: usize) -> bool {
    !name.is_empty()
        // len() 数 UTF-8 字节；由于这里只允许 ASCII，字节数就是字符数。
        && name.len() <= max
        && name
            .bytes()
            // all 要求每一个字节都合法；b'_' 和 b'-' 是字节字面量。
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// 这是同步的 CPU 计算；把它写成 async 也不会自动使计算让出线程。
fn password_hash(password: &str, salt: &[u8; 16]) -> [u8; 32] {
    // 先准备输出数组；&mut 允许 PBKDF2 把结果写入数组。
    let mut output = [0; 32];
    // 用密码的 UTF-8 字节和盐迭代 100,000 次，生成 SHA-256 密码摘要。
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, 100_000, &mut output);
    // 没有分号的最后一个表达式就是函数的返回值。
    output
}

/// 生成不可预测的令牌：32 个随机字节 → 64 个十六进制字符。
fn new_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    // map 把每个字节转成两位十六进制（不足补 0）；collect 拼成 String。
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// impl 给 Service 定义方法。
impl Service {
    /// 业务入口；借用参数，不取得调用方数据的所有权。
    pub fn handle(
        // &self 是当前服务；method/path 是字符串切片，body 是已经解析的 JSON。
        &self,
        method: &str,
        path: &str,
        body: &Value,
        authorization: &str,
    ) -> (u16, Value) {
        // if let 只在 route_error 返回 Some 时进入；return 提前结束请求。
        if let Some(status) = route_error(method, path) {
            return error(
                status,
                if status == 404 {
                    "Not found"
                } else {
                    "Method not allowed"
                },
            );
        }
        // ping 公开，不需要账号、请求体或令牌。
        if method == "GET" && path == "/ping" {
            return (200, json!({"data": "pong"}));
        }
        // 注册和登录复用字段校验；matches! 检查路径是否属于列出的任一项。
        if method == "POST" && matches!(path, "/users" | "/sessions") {
            // get 返回 Option；and_then 在有字段时继续检查它是否为字符串。
            // let ... else：没有拿到字符串就返回 400，否则 name 是借用的 &str。
            let Some(name) = body.get("username").and_then(Value::as_str) else {
                return error(400, "Expected username");
            };
            // 密码同样必须存在，且为 JSON 字符串。
            let Some(password) = body.get("password").and_then(Value::as_str) else {
                return error(400, "Expected password");
            };
            // 只允许 username/password 两个字段；|| 表示任一条件不合法即拒绝。
            if body.as_object().map(|v| v.len()) != Some(2)
                || !valid_name(name, 32)
                // chars().count() 数 Unicode 标量值；8..=128 两端都包含。
                || !(8..=128).contains(&password.chars().count())
            {
                return error(400, "Invalid account fields");
            }
            // /users 走注册分支；/sessions 会跳过它，进入下面的登录逻辑。
            if path == "/users" {
                let mut salt = [0; 16];
                OsRng.fill_bytes(&mut salt);
                // 耗时计算在加锁前完成，减少其他请求等待用户表的时间。
                let digest = password_hash(password, &salt);
                // lock 获取守卫（MutexGuard）；守卫离开作用域时自动释放锁。
                // unwrap 假定锁未被毒化；若其他线程持锁时 panic，这里也可能 panic。
                let mut users = self.users.lock().unwrap();
                // 查重和插入在同一把锁里，两个同名注册不能都成功。
                if users.contains_key(name) {
                    return error(409, "Username exists");
                }
                // name.into() 把借用的 &str 转为拥有数据的 String，作为表的键。
                users.insert(
                    name.into(),
                    User {
                        salt,
                        digest,
                        // 注册不自动登录，文本表也从空开始。
                        token: None,
                        texts: BTreeMap::new(),
                    },
                );
                // 201 表示创建成功；这个响应返回用户名，GET /texts 不返回用户名。
                return (201, json!({"data": {"username": name}}));
            }
            // 登录先在短小的锁作用域内读取盐和摘要。
            let (salt, expected) = {
                let users = self.users.lock().unwrap();
                // 不存在的用户名和错误密码都返回 401，不细分对外错误。
                let Some(user) = users.get(name) else {
                    return error(401, "Invalid username or password");
                };
                // 两个字节数组可以复制；没有把 user 的引用带到锁外。
                (user.salt, user.digest)
            };
            // 上面块结束，锁已释放；其他请求可以在密码计算期间访问用户表。
            let digest = password_hash(password, &salt);
            // 修改登录状态前重新加锁，也重新检查账号还在不在。
            let mut users = self.users.lock().unwrap();
            let Some(user) = users.get_mut(name) else {
                return error(401, "Invalid username or password");
            };
            // 检查盐是否变化，防止旧账号的登录作用于注销后同名重注册的账号。
            // ct_eq 比较摘要并返回 Choice；bool::from 将结果转成 bool，! 表示不匹配。
            if user.salt != salt || !bool::from(digest.ct_eq(&expected)) {
                return error(401, "Invalid username or password");
            }
            let token = new_token();
            // clone 留一份给响应；表里保存另一份，旧令牌随赋值失效。
            user.token = Some(token.clone());
            // 待完成：记录令牌到期时间，并在成功响应中增加 expires_in。
            return (200, json!({"data": {"token": token}}));
        }
        // 当前需要登录的两条路由；以后增加文本路由和注销时也须纳入鉴权。
        let protected = matches!(path, "/texts" | "/sessions/current");
        if protected {
            // 请求头格式为 Authorization: Bearer <token>；前缀不对就当作无令牌。
            let token = authorization.strip_prefix("Bearer ").unwrap_or("");
            let mut users = self.users.lock().unwrap();
            // 在用户表中查找令牌所属用户，身份由令牌确定，不由客户端指定。
            let name = users
                .iter()
                // as_deref 把 Option<String> 变为 Option<&str>，避免复制令牌。
                .find(|(_, user)| !token.is_empty() && user.token.as_deref() == Some(token))
                // 复制用户名，结束遍历的不可变借用，之后才能 get_mut 可变借用。
                .map(|(name, _)| name.clone());
            let Some(name) = name else {
                return error(401, "Login required");
            };
            // 刚才在同一把锁下找到了用户，所以这里一定存在。
            let user = users.get_mut(&name).unwrap();
            // 待完成：检查到期时间。鉴权和后续数据操作应始终在同一锁作用域内。
            // 这里没有 await；不会持着用户表的锁去等待网络。
            if method == "DELETE" && path == "/sessions/current" {
                // 清空服务端令牌，客户端即使还保存旧值也无法再次通过鉴权。
                user.token = None;
                // JSON 的 null 表示操作成功但没有要返回的数据。
                return (200, json!({"data": null}));
            }
            if method == "GET" && path == "/texts" {
                // keys 只取文本名称；BTreeMap 保证升序，collect 收集成数组供 JSON 序列化。
                // 注册时 texts 为空，且起始服务端还没有 PUT 路由，所以此处返回 []。
                return (200, json!({"data": user.texts.keys().collect::<Vec<_>>()}));
            }
        }
        // 防御性兜底：路由表里声明了接口，却没有对应业务分支时返回 404。
        error(404, "Not found")
    }
}

// cfg(test) 使下面模块只在测试构建时参与编译。
#[cfg(test)]
mod tests {
    // super 是父模块；引入其中的 Service、json! 等供测试使用。
    use super::*;
    // 验证注册、重复注册、登录替换令牌、空文本列表和退出撤销令牌。
    // #[test] 告诉 cargo test 把这个函数作为测试运行。
    #[test]
    fn account_lifecycle() {
        // 每个测试使用独立的空状态，避免依赖运行中的服务端。
        let service = Service::default();
        let account = json!({"username":"alice", "password":"password1"});
        // .0 取返回元组的状态码；assert_eq! 在不相等时让测试失败。
        assert_eq!(service.handle("POST", "/users", &account, "").0, 201);
        assert_eq!(service.handle("POST", "/users", &account, "").0, 409);
        // .1 取 JSON 响应体；读取登录令牌，组装鉴权请求头的值。
        let login = service.handle("POST", "/sessions", &account, "").1;
        let old = format!("Bearer {}", login["data"]["token"].as_str().unwrap());
        // 再次登录应产生新令牌，并替换旧令牌。
        let login = service.handle("POST", "/sessions", &account, "").1;
        let current = format!("Bearer {}", login["data"]["token"].as_str().unwrap());
        assert_ne!(old, current);
        assert_eq!(service.handle("GET", "/texts", &Value::Null, &old).0, 401);
        // 登录成功但尚无文本时，空列表是正确结果，而不是缺少用户名。
        assert_eq!(
            service.handle("GET", "/texts", &Value::Null, &current),
            (200, json!({"data":[]}))
        );
        // 退出成功后，同一个令牌再访问文本列表必须得到 401。
        assert_eq!(
            service
                .handle("DELETE", "/sessions/current", &Value::Null, &current)
                .0,
            200
        );
        assert_eq!(
            service.handle("GET", "/texts", &Value::Null, &current).0,
            401
        );
    }
}
