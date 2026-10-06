//! 命令行入口：读取用户命令，调用 `exchange`，并维护本地登录令牌。

use clap::Parser;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::io::{self, Write};
use std::time::Duration;

// clap 根据这个结构体生成 --url 参数的解析逻辑。
#[derive(Parser)]
struct Args {
    // 未传 --url 时连接本机服务端；这里只保存基础地址，具体路径由命令决定。
    #[arg(long, default_value = "http://127.0.0.1:7878")]
    url: String,
}

/// 显示提示并读取一行输入；返回不含行尾换行符的 String。
fn input(prompt: &str) -> io::Result<String> {
    print!("{prompt}");
    // print! 不保证立即显示，先刷新输出缓冲区，用户才能看到提示再输入。
    io::stdout().flush()?;
    let mut line = String::new();
    // read_line 返回读取的字节数；0 表示输入结束（EOF），不是空字符串命令。
    if io::stdin().read_line(&mut line)? == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    // 只去掉行尾的 \r/\n，保留用户输入的其他字符。
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

//读取echo的文本
fn read_echo_text() -> io::Result<String> {
    let mut line: Vec<String> = Vec::new();
    loop {
        //读取一行输入
        let mut text = input("|")?;
        if text == "." {
            break;
        }
        //当以两个点结尾时，删除最后一个点
        if text.ends_with("..") {
            let _ = text.pop();
        }
        line.push(text);
    }
    Ok(line.join("\n"))
}


// Box<dyn Error> 让 main 可以用 ? 传播来自输入、HTTP 客户端等不同类型的错误。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    // blocking::Client 在当前线程等待网络操作；复用它可以复用连接。
    let client = Client::builder()
        // 避免服务端无响应时一直等待；禁用环境代理，默认连接本机服务端。
        .timeout(Duration::from_secs(12))
        .no_proxy()
        // 直接显示 3xx 响应，不自动跟随重定向。
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    // 空字符串表示当前没有可用的登录令牌。
    let mut token = String::new();
    loop {
        // EOF 正常退出交互循环；其他输入错误交给 main 的调用者处理。
        let command = match input(
            "ping / register / login / logout / list / echo / delete-user / put / get / delete / q > ",
        ) {
            Ok(command) => command,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        };
        // Null 作为“此命令没有请求体”的临时标记；注册和登录会改成 JSON 对象。
        let mut body = Value::Null;
        // as_str() 借用 String 中的文本；match 把命令映射成 HTTP 方法和路径。
        let (method, path) = match command.as_str() {
            "q" => break,
            "ping" => ("GET", "/ping"),
            "list" => ("GET", "/texts"),
            "logout" => ("DELETE", "/sessions/current"),
            "register" | "login" => {
                // json! 构造 JSON 请求体；密码由 rpassword 读取，输入时不会回显。
                body = json!({"username": input("username: ")?, "password": rpassword::prompt_password("password: ")?});
                (
                    "POST",
                    if command == "register" {
                        "/users"
                    } else {
                        "/sessions"
                    },
                )
            }

            //echo的接入实现
            "echo" => {
                let text = read_echo_text()?;
                body = json!({"text": text});
                ("POST", "/echo")
            }


            "delete-user" | "put" | "get" | "delete" => {
                // 起始代码尚未实现这些命令，所以不发送 HTTP 请求。
                println!("This task is not implemented in the starting code yet.");
                continue;
            }
            _ => {
                println!("Unknown command.");
                continue;
            }
        };
        // 二进制入口调用库中的请求函数；固定的 GET/POST/DELETE 字符串可解析为 Method。
        // &token 是借用，exchange 读取令牌但不取得其所有权；无请求体时传 None。
        let result = rm_client_sync::exchange(
            &client,
            &args.url,
            method.parse().unwrap(),
            path,
            &token,
            if body.is_null() { None } else { Some(&body) },
        );
        match result {
            Ok((status, value)) => {
                if command == "echo" && status == 200 {
                    println!("HTTP {status}");
                
                    if let Some(reply) = value["data"].as_str() {
                        print!("{reply}");
                        // 如果响应文本本身没有以换行结束，就另加一个换行，
                        // 让下一次命令提示符从新的一行开始。
                        if !reply.ends_with('\n') {
                            println!();
                        }
                    } else {
                        // 响应格式不符合预期时，显示整个 JSON，方便排查。
                        println!("{value}");
                    }
                } else {
                    println!("{status} {value}");
                }
                // 仅在登录成功且响应中确实有字符串 token 时，更新本地令牌。
                // JSON 索引缺失时会得到 Null，as_str() 随之返回 None。
                if command == "login"
                    && status == 200
                    && let Some(next) = value["data"]["token"].as_str()
                {
                    // next 是从响应中借用的 &str；转换成 String 后可独立保存在 token 中。
                    token = next.into();
                }
                if status == 401 {
                    println!("Please log in again.");
                }
                // 当前实现收到任何 401 都会清空令牌；成功退出登录也会清空。
                if status == 401 || (command == "logout" && status == 200) {
                    token.clear();
                }
            }
            // 网络、超时或响应读取错误走这里，不会被误认为 HTTP 状态码。
            Err(error) => eprintln!("Request failed: {error}"),
        }
    }
    Ok(())
}
