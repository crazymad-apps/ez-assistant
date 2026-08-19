//! 最小命令行对话 demo：直接驱动 [`OpenAiCompatibleService`] 与 DeepSeek 真实 API 对话。
//!
//! 运行（需要仓库根 `.env` 中的 `DEEPSEEK_API_KEY`，模板见 crate 文档）：
//!
//! ```bash
//! cargo run -p agent-openai-compatible --example chat
//! ```
//!
//! 环境变量：`DEEPSEEK_API_KEY`（必需）、`DEEPSEEK_BASE_URL`（可选，缺省官方
//! endpoint）、`DEEPSEEK_MODEL`（可选，缺省 `deepseek-v4-flash`）、
//! `DEEPSEEK_CONTEXT_WINDOW_TOKENS`（可选，缺省 `128000`）。
//! 输入一行后回车发送，reasoning 以暗色显示；`/quit` 或 Ctrl-D 退出。
//!
//! 调试：`--debug <url>`（或环境变量 `DEBUG_URL`）把每次 Turn 的请求快照、建立信息
//! 与全部模型事件推送到 debug viewer（先运行 `cargo run -p debug-viewer`，浏览器打开
//! http://localhost:7331）；viewer 未启动时推送自动静音，不影响对话。

use std::io::{BufRead, Write};
use std::time::Instant;

use agent_model::{
    GenerationConfig, ModelCallContext, ModelEvent, ModelRequest, ModelService, ProviderOptions,
    SystemPromptSnapshot,
};
use agent_openai_compatible::{
    BearerCredential, OpenAiCompatibleService, ProtocolAdapter, TransportTimeouts,
};
use agent_types::{
    ConversationMessage, ConversationSnapshot, MessageId, PartId, TextPart, ToolChoice,
    UserMessage, UserPart,
};
use debug_viewer::{DebugClient, DebugPayload};
use futures_util::StreamExt;

/// reasoning 文本的暗色显示与恢复。
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("DEEPSEEK_API_KEY")
        .map_err(|_| "缺少 DEEPSEEK_API_KEY：请在仓库根 .env 中配置")?;
    let base_url = std::env::var("DEEPSEEK_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "https://api.deepseek.com".to_owned());
    let model = std::env::var("DEEPSEEK_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "deepseek-v4-flash".to_owned());
    let context_window_tokens = std::env::var("DEEPSEEK_CONTEXT_WINDOW_TOKENS")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.parse::<u64>())
        .transpose()
        .map_err(|_| "DEEPSEEK_CONTEXT_WINDOW_TOKENS 必须是正整数")?
        .unwrap_or(128_000);
    if context_window_tokens == 0 {
        return Err("DEEPSEEK_CONTEXT_WINDOW_TOKENS 必须大于 0".into());
    }

    let service = OpenAiCompatibleService::new(
        base_url.clone(),
        BearerCredential::new(api_key),
        model.clone(),
        context_window_tokens,
        ProtocolAdapter::deepseek(),
        TransportTimeouts::default(),
    )?;
    println!("DeepSeek 命令行 demo（模型：{model}）。输入回车发送，/quit 或 Ctrl-D 退出。");

    // --debug <url> 优先，其次环境变量 DEBUG_URL；都缺省则不推送调试数据。
    let debug_url = debug_url_from_args().or_else(|| std::env::var("DEBUG_URL").ok());
    let debug: Option<DebugClient> = debug_url.map(|url| {
        eprintln!("[debug] 推送调试数据到 {url}");
        DebugClient::new(url).with_correlation_id(format!("chat-{}", std::process::id()))
    });

    let stdin = std::io::stdin();
    let mut messages: Vec<ConversationMessage> = Vec::new();
    let mut turn = 0_u32;
    loop {
        print!("> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            println!();
            break; // Ctrl-D
        }
        let input = line.trim();
        if input.is_empty() || input == "/quit" {
            if input == "/quit" {
                break;
            }
            continue;
        }
        turn += 1;
        messages.push(ConversationMessage::User(UserMessage {
            id: MessageId::new(format!("user_{turn}"))?,
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new(format!("user_{turn}_text"))?,
                text: input.to_owned(),
            })],
        }));

        // thinking 开关经 deepseek 命名空间下发，编码器按命名空间合并进请求根（M6 契约）。
        let mut provider_options = ProviderOptions::new();
        provider_options.insert(
            "deepseek",
            serde_json::json!({"thinking": {"type": "enabled"}}),
        )?;
        let request = ModelRequest {
            system: SystemPromptSnapshot::default(),
            conversation: ConversationSnapshot::new(messages.clone()),
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            generation: GenerationConfig::default(),
            reasoning: None,
            provider_options,
        };
        if let Some(client) = &debug {
            client.post(DebugPayload::TurnRequested {
                request: request.clone(),
            });
        }
        let establish_started = Instant::now();
        let stream_result = service.stream(request, ModelCallContext::default()).await;
        let mut stream = match stream_result {
            Ok(stream) => {
                if let Some(client) = &debug {
                    client.post(DebugPayload::TurnEstablished {
                        model: model.clone(),
                        endpoint: base_url.clone(),
                        message_count: messages.len() as u32,
                        tool_count: 0,
                        elapsed_ms: establish_started.elapsed().as_millis() as u64,
                    });
                }
                stream
            }
            Err(error) => {
                if let Some(client) = &debug {
                    client.post(DebugPayload::EstablishmentFailed {
                        error: error.to_string(),
                    });
                }
                messages.pop();
                eprintln!("{RESET}请求建立失败：{error}");
                continue;
            }
        };

        let mut finished = None;
        while let Some(event) = stream.next().await {
            if let Some(client) = &debug {
                client.post(DebugPayload::ModelEvent {
                    event: event.clone(),
                });
            }
            match event {
                ModelEvent::TurnStarted { .. } => {}
                ModelEvent::ReasoningStarted { .. } => print!("{DIM}"),
                ModelEvent::ReasoningDelta { delta, .. } => print!("{delta}"),
                ModelEvent::ReasoningFinished { .. } => println!("{RESET}"),
                ModelEvent::TextStarted { .. } => {}
                ModelEvent::TextDelta { delta, .. } => print!("{delta}"),
                ModelEvent::TextFinished { .. } => {}
                ModelEvent::ToolCallStarted { name, .. } => {
                    print!("\n[tool call: {}]", name.as_str());
                }
                ModelEvent::ToolCallDelta { .. } => {}
                ModelEvent::ToolCallFinished { .. } => println!(),
                ModelEvent::UsageUpdated { .. } => {}
                ModelEvent::TurnFinished { message } => {
                    println!();
                    if let Some(usage) = &message.usage {
                        let cached = usage
                            .cached_input_tokens
                            .map(|value| format!(", cached {value}"))
                            .unwrap_or_default();
                        println!(
                            "{DIM}(tokens: {} in / {} out{cached}){RESET}",
                            usage.input_tokens, usage.output_tokens
                        );
                    }
                    finished = Some(message);
                }
                ModelEvent::TurnFailed { error } => {
                    println!("{RESET}");
                    eprintln!("turn 失败：{error}");
                }
            }
            std::io::stdout().flush()?;
        }
        match finished {
            // 只把完整完成的 assistant 消息纳入历史；失败的轮次连用户输入一起撤回。
            Some(message) => messages.push(ConversationMessage::Assistant(message)),
            None => {
                messages.pop();
            }
        }
    }
    Ok(())
}

/// 从命令行参数解析 `--debug <url>` 或 `--debug=<url>`。
fn debug_url_from_args() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--debug" {
            return args.next();
        }
        if let Some(url) = arg.strip_prefix("--debug=") {
            return Some(url.to_owned());
        }
    }
    None
}
