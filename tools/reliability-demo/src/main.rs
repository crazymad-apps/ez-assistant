//! 完整模型交互录制、有限重试与分层回放的独立验证宿主。
//!
//! 该 binary 由开发者显式启动，不属于正式 Assistant Runtime 或桌面应用。

use std::{
    collections::BTreeSet,
    env,
    num::{NonZeroU64, NonZeroUsize},
    path::PathBuf,
    time::Duration,
};

use agent_model::{ModelRetryPolicy, ModelRetryReason};

mod collector;
mod model_replay;
mod record;
mod replay;
mod timeline;
mod trace;
mod wire_replay;

const HELP: &str = "\
Reliability Demo - v0.6.0 可靠性能力验证宿主

USAGE:
    reliability-demo --help
    reliability-demo record --data-dir <DIR> [RETRY OPTIONS]
    reliability-demo timeline --trace <FILE> [--full]
    reliability-demo replay-wire --trace <FILE>
    reliability-demo replay-model --trace <FILE>

COMMANDS:
    record         从标准输入读取任务，使用真实 DeepSeek Provider 生成 Complete Trace
    timeline       只读展示 Complete 或 Incomplete Trace；默认隐藏高敏正文
    replay-wire    离线重放 Provider wire 并重新经过真实 Decoder
    replay-model   离线重放规范 ModelService 边界

RECORD OPTIONS:
    --trace-queue-capacity <N>   Trace 有界队列容量（默认 4096）
    --max-trace-bytes <N>        Trace 最大字节数（默认 67108864）
    --retry-on <REASON>          可重复：connection|timeout|rate-limited|unavailable
    --retry-delay-ms <MS>        每次失败后的等待；可重复，数量决定最大 attempt
    --max-retry-after-ms <MS>    启用重试时必传的 Provider Retry-After 上限

只有 record 加载 .env、DEEPSEEK_API_KEY 并创建网络 Transport。读取命令不读取
credential、不访问网络或执行工具。Trace 包含完整高敏模型交互，请使用专用目录。
";

#[tokio::main]
async fn main() {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments.is_empty()
        || matches!(arguments.as_slice(), [flag] if flag == "--help" || flag == "-h")
    {
        println!("{HELP}");
        return;
    }

    match parse_command(&arguments) {
        Ok(Command::Record(options)) => {
            if let Err(error) = record::run(options).await {
                exit_with_error(&error.to_string());
            }
        }
        Ok(Command::Timeline { trace, full }) => match trace::load_for_timeline(&trace).await {
            Ok(trace) => print!("{}", timeline::render_timeline(&trace, full)),
            Err(error) => exit_with_error(&error.to_string()),
        },
        Ok(Command::ReplayWire { trace }) => match trace::load_complete(&trace).await {
            Ok(trace) => match wire_replay::run_wire_replay(&trace).await {
                Ok(attempts) => println!("Wire Replay 完成：{attempts} 个 attempt"),
                Err(error) => exit_with_error(&error.to_string()),
            },
            Err(error) => exit_with_error(&error.to_string()),
        },
        Ok(Command::ReplayModel { trace }) => match trace::load_complete(&trace).await {
            Ok(trace) => match model_replay::run_model_replay(&trace).await {
                Ok(calls) => println!("Model Replay 完成：{calls} 个逻辑调用"),
                Err(error) => exit_with_error(&error.to_string()),
            },
            Err(error) => exit_with_error(&error.to_string()),
        },
        Err(error) => exit_with_error(error),
    }
}

enum Command {
    Record(record::RecordOptions),
    Timeline { trace: PathBuf, full: bool },
    ReplayWire { trace: PathBuf },
    ReplayModel { trace: PathBuf },
}

fn parse_command(arguments: &[String]) -> Result<Command, &'static str> {
    let [command, rest @ ..] = arguments else {
        return Err("缺少命令；请使用 `reliability-demo --help`");
    };
    if command == "record" {
        return parse_record_command(rest).map(Command::Record);
    }
    let mut trace = None;
    let mut full = false;
    let mut index = 0;
    while index < rest.len() {
        match rest[index].as_str() {
            "--trace" if trace.is_none() => {
                index += 1;
                let Some(value) = rest.get(index) else {
                    return Err("--trace 必须提供文件路径");
                };
                trace = Some(PathBuf::from(value));
            }
            "--full" if command == "timeline" && !full => full = true,
            _ => return Err("命令参数无效；请使用 `reliability-demo --help`"),
        }
        index += 1;
    }
    let trace = trace.ok_or("命令必须提供 --trace <FILE>")?;
    match command.as_str() {
        "timeline" => Ok(Command::Timeline { trace, full }),
        "replay-wire" => Ok(Command::ReplayWire { trace }),
        "replay-model" => Ok(Command::ReplayModel { trace }),
        _ => Err("未知命令；请使用 `reliability-demo --help`"),
    }
}

fn parse_record_command(arguments: &[String]) -> Result<record::RecordOptions, &'static str> {
    let defaults = collector::CollectorConfig::default();
    let mut data_dir = None;
    let mut queue_capacity = defaults.queue_capacity;
    let mut max_trace_bytes = defaults.max_trace_bytes;
    let mut retry_on = BTreeSet::new();
    let mut retry_delays = Vec::new();
    let mut max_retry_after = None;
    let mut index = 0;
    while index < arguments.len() {
        let option = arguments[index].as_str();
        index += 1;
        let value = arguments
            .get(index)
            .ok_or("record 选项缺少参数值；请使用 `reliability-demo --help`")?;
        match option {
            "--data-dir" if data_dir.is_none() => data_dir = Some(PathBuf::from(value)),
            "--trace-queue-capacity" => {
                queue_capacity = parse_non_zero_usize(value, "--trace-queue-capacity")?;
            }
            "--max-trace-bytes" => {
                max_trace_bytes = parse_non_zero_u64(value, "--max-trace-bytes")?;
            }
            "--retry-on" => {
                retry_on.insert(parse_retry_reason(value)?);
            }
            "--retry-delay-ms" => {
                retry_delays.push(Duration::from_millis(parse_u64(value, "--retry-delay-ms")?));
            }
            "--max-retry-after-ms" if max_retry_after.is_none() => {
                max_retry_after = Some(Duration::from_millis(parse_u64(
                    value,
                    "--max-retry-after-ms",
                )?));
            }
            _ => return Err("record 命令参数无效或重复；请使用 `reliability-demo --help`"),
        }
        index += 1;
    }
    let data_dir = data_dir.ok_or("record 必须提供 --data-dir <DIR>")?;
    if data_dir.as_os_str().is_empty() {
        return Err("--data-dir 不能为空");
    }
    let retry_configured =
        !retry_on.is_empty() || !retry_delays.is_empty() || max_retry_after.is_some();
    let retry_policy = if retry_configured {
        if retry_on.is_empty() || retry_delays.is_empty() || max_retry_after.is_none() {
            return Err(
                "启用重试必须同时提供 --retry-on、--retry-delay-ms 和 --max-retry-after-ms",
            );
        }
        Some(ModelRetryPolicy::new(
            retry_on,
            retry_delays,
            max_retry_after.expect("presence was checked"),
        ))
    } else {
        None
    };
    Ok(record::RecordOptions {
        data_dir,
        collector: collector::CollectorConfig {
            queue_capacity,
            max_trace_bytes,
        },
        retry_policy,
    })
}

fn parse_non_zero_usize(value: &str, name: &'static str) -> Result<NonZeroUsize, &'static str> {
    value
        .parse::<usize>()
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or(match name {
            "--trace-queue-capacity" => "--trace-queue-capacity 必须是正整数",
            _ => "参数必须是正整数",
        })
}

fn parse_non_zero_u64(value: &str, name: &'static str) -> Result<NonZeroU64, &'static str> {
    value
        .parse::<u64>()
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or(match name {
            "--max-trace-bytes" => "--max-trace-bytes 必须是正整数",
            _ => "参数必须是正整数",
        })
}

fn parse_u64(value: &str, name: &'static str) -> Result<u64, &'static str> {
    value.parse::<u64>().map_err(|_| match name {
        "--retry-delay-ms" => "--retry-delay-ms 必须是非负整数",
        "--max-retry-after-ms" => "--max-retry-after-ms 必须是非负整数",
        _ => "参数必须是非负整数",
    })
}

fn parse_retry_reason(value: &str) -> Result<ModelRetryReason, &'static str> {
    match value {
        "connection" => Ok(ModelRetryReason::Connection),
        "timeout" => Ok(ModelRetryReason::Timeout),
        "rate-limited" => Ok(ModelRetryReason::RateLimited),
        "unavailable" => Ok(ModelRetryReason::Unavailable),
        _ => Err("--retry-on 只接受 connection|timeout|rate-limited|unavailable"),
    }
}

fn exit_with_error(message: &str) -> ! {
    eprintln!("Reliability Demo 失败：{message}");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timeline_without_loading_environment_or_network() {
        let command = parse_command(&[
            "timeline".into(),
            "--trace".into(),
            "fixture.jsonl".into(),
            "--full".into(),
        ])
        .unwrap();
        let Command::Timeline { trace, full } = command else {
            panic!("expected timeline command")
        };
        assert_eq!(trace, PathBuf::from("fixture.jsonl"));
        assert!(full);
    }

    #[test]
    fn rejects_unimplemented_and_incomplete_commands() {
        assert!(parse_command(&["record".into()]).is_err());
        assert!(parse_command(&["timeline".into()]).is_err());
        assert!(parse_command(&["timeline".into(), "--full".into()]).is_err());
    }

    #[test]
    fn parses_record_defaults_without_touching_environment() {
        let command = parse_command(&["record".into(), "--data-dir".into(), "trace-data".into()])
            .expect("valid record command");
        let Command::Record(options) = command else {
            panic!("expected record command")
        };
        assert_eq!(options.data_dir, PathBuf::from("trace-data"));
        assert_eq!(options.collector, collector::CollectorConfig::default());
        assert!(options.retry_policy.is_none());
    }

    #[test]
    fn parses_explicit_retry_and_rejects_partial_configuration() {
        let command = parse_command(&[
            "record".into(),
            "--data-dir".into(),
            "trace-data".into(),
            "--retry-on".into(),
            "timeout".into(),
            "--retry-delay-ms".into(),
            "25".into(),
            "--retry-delay-ms".into(),
            "50".into(),
            "--max-retry-after-ms".into(),
            "500".into(),
        ])
        .expect("valid retry configuration");
        let Command::Record(options) = command else {
            panic!("expected record command")
        };
        let policy = options.retry_policy.expect("retry should be enabled");
        assert_eq!(
            policy.delays,
            [Duration::from_millis(25), Duration::from_millis(50)]
        );
        assert_eq!(policy.max_retry_after, Duration::from_millis(500));
        assert_eq!(policy.retry_on, BTreeSet::from([ModelRetryReason::Timeout]));

        assert!(
            parse_command(&[
                "record".into(),
                "--data-dir".into(),
                "trace-data".into(),
                "--retry-on".into(),
                "timeout".into(),
            ])
            .is_err()
        );
    }

    #[test]
    fn rejects_invalid_record_capacity_and_unknown_retry_reason() {
        assert!(
            parse_command(&[
                "record".into(),
                "--data-dir".into(),
                "trace-data".into(),
                "--trace-queue-capacity".into(),
                "0".into(),
            ])
            .is_err()
        );
        assert!(
            parse_command(&[
                "record".into(),
                "--data-dir".into(),
                "trace-data".into(),
                "--retry-on".into(),
                "maybe".into(),
            ])
            .is_err()
        );
    }

    #[test]
    fn parses_both_replay_commands_without_full_flag() {
        assert!(matches!(
            parse_command(&[
                "replay-wire".into(),
                "--trace".into(),
                "fixture.jsonl".into()
            ]),
            Ok(Command::ReplayWire { .. })
        ));
        assert!(matches!(
            parse_command(&[
                "replay-model".into(),
                "--trace".into(),
                "fixture.jsonl".into()
            ]),
            Ok(Command::ReplayModel { .. })
        ));
        assert!(
            parse_command(&[
                "replay-wire".into(),
                "--trace".into(),
                "fixture.jsonl".into(),
                "--full".into()
            ])
            .is_err()
        );
    }
}
