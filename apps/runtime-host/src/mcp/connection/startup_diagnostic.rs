//! 启动 stderr 只识别已知配置错误，转换成固定中文建议；原文、路径及凭据不出 Host。

use assistant_protocol::McpServerSource;
use assistant_runtime::McpConnectionError;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::watch,
};

const MAX_LINE_BYTES: usize = 8192;

pub(super) fn with_source(
    source: &McpServerSource,
    error: McpConnectionError,
) -> McpConnectionError {
    let source = match source {
        McpServerSource::User => "用户添加".to_owned(),
        McpServerSource::Plugin(name) => format!("插件 · {name}"),
        McpServerSource::Builtin => "内置资源".to_owned(),
    };
    McpConnectionError::new(error.kind(), format!("来源：{source}；{}", error.message()))
}

pub(super) async fn drain(
    mut stderr: impl AsyncRead + Unpin,
    diagnostic: watch::Sender<Option<&'static str>>,
) {
    let mut buffer = [0_u8; 8192];
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) => {
                if !oversized {
                    publish(&line, &diagnostic);
                }
                return;
            }
            Err(_) => return,
            Ok(size) => {
                for byte in &buffer[..size] {
                    if *byte == b'\n' {
                        if !oversized {
                            publish(&line, &diagnostic);
                        }
                        line.clear();
                        oversized = false;
                    } else if line.len() < MAX_LINE_BYTES {
                        line.push(*byte);
                    } else {
                        oversized = true;
                    }
                }
            }
        }
    }
}

fn publish(line: &[u8], diagnostic: &watch::Sender<Option<&'static str>>) {
    if let Ok(line) = std::str::from_utf8(line)
        && let Some(message) = classify(line.trim())
    {
        diagnostic.send_replace(Some(message));
    }
}

fn classify(line: &str) -> Option<&'static str> {
    // 旧版插件启动前校验原生 JSON，错误未进入 MCP wire。只匹配完整已知文本，
    // 不透传任意 stderr，也不根据片段猜测配置缺失；可识别集合之外返回通用中文诊断。
    let reason = line.strip_prefix("deepseek-search-mcp: ")?;
    Some(match reason {
        "请在通用辅助的原生配置文件中上传 deepseek-search.json 并填写 api_key，随后刷新 MCP。" => {
            "缺少插件原生配置文件 deepseek-search.json。请在插件配置中添加该文件并填写 api_key，然后刷新此服务；内置 DeepSeek 的环境变量配置不会用于该插件。"
        }
        "deepseek-search.json 配置缺失或无效：api_key" => {
            "插件原生配置文件 deepseek-search.json 中的 api_key 缺失或无效，请填写有效的 API Key 后刷新此服务。"
        }
        "deepseek-search.json 不是有效 JSON。" => {
            "插件原生配置文件 deepseek-search.json 的 JSON 格式错误，请修正后刷新此服务。"
        }
        "deepseek-search.json 超过 64 KiB 限制。" => {
            "插件原生配置文件 deepseek-search.json 超过 64 KiB 大小限制。"
        }
        "无法读取 deepseek-search.json。" => {
            "无法读取插件原生配置文件 deepseek-search.json，请检查文件权限和 UTF-8 编码。"
        }
        "缺少 Host 提供的 EZ_ASSISTANT_PLUGIN_CONFIG_DIR。" => {
            "缺少插件配置目录环境变量 EZ_ASSISTANT_PLUGIN_CONFIG_DIR，请检查插件启动配置。"
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_failure_names_exact_source_even_when_server_keys_match() {
        use assistant_runtime::McpConnectionFailureKind;
        for (source, expected) in [
            (McpServerSource::User, "用户添加"),
            (
                McpServerSource::Plugin("general-assistant".into()),
                "插件 · general-assistant",
            ),
            (McpServerSource::Builtin, "内置资源"),
        ] {
            let error = with_source(
                &source,
                McpConnectionError::new(McpConnectionFailureKind::Protocol, "连接初始化失败"),
            );
            assert_eq!(error.message(), format!("来源：{expected}；连接初始化失败"));
        }
    }

    #[tokio::test]
    async fn known_configuration_errors_are_projected_without_raw_stderr() {
        let (sender, receiver) = watch::channel(None);
        let input = "secret-token=https://private.invalid\ndeepseek-search-mcp: 请在通用辅助的原生配置文件中上传 deepseek-search.json 并填写 api_key，随后刷新 MCP。\n";
        drain(input.as_bytes(), sender).await;
        let message = receiver.borrow().expect("configuration error");
        assert!(message.contains("缺少插件原生配置文件 deepseek-search.json"));
        assert!(message.contains("api_key"));
        assert!(!message.contains("secret-token"));
        assert!(!message.contains("private.invalid"));
    }

    #[tokio::test]
    async fn unknown_or_oversized_stderr_is_drained_without_disclosure() {
        let (sender, receiver) = watch::channel(None);
        let input = format!(
            "{}\ndeepseek-search-mcp: deepseek-search.json 配置缺失或无效：secret-token\n",
            "x".repeat(1024 * 1024)
        );
        drain(input.as_bytes(), sender).await;
        assert!(receiver.borrow().is_none());
    }

    #[tokio::test]
    async fn split_utf8_lines_and_missing_final_newline_are_supported() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(1);
        let (sender, receiver) = watch::channel(None);
        let output = async move {
            writer
                .write_all(
                    "deepseek-search-mcp: deepseek-search.json 配置缺失或无效：api_key".as_bytes(),
                )
                .await
                .unwrap();
        };
        tokio::join!(output, drain(reader, sender));
        assert!(receiver.borrow().unwrap().contains("api_key 缺失或无效"));
    }
}
