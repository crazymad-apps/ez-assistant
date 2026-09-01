//! 为 Desktop 生成单一、只读的 TypeScript 协议绑定。

use std::{env, error::Error, fs, path::PathBuf, process};

use assistant_protocol::{
    DeviceGatewayCommand, DeviceGatewayCommandResult, DeviceGatewayEvent,
    GetApplicationSnapshotRequest, GetApplicationSnapshotResult, GetChildTaskViewRequest,
    GetChildTaskViewResult, GetConversationPageAroundRunRequest,
    GetConversationPageAroundRunResult, GetSessionViewRequest, GetSessionViewResult,
    GetToolDetailRequest, GetToolDetailResult, GoalId, InterruptRunRequest, InterruptRunResult,
    ListConversationPageRequest, ListConversationPageResult, PrioritizeQueuedInputRequest,
    PrioritizeQueuedInputResult, RejectApprovalAndStopRunRequest, RejectApprovalAndStopRunResult,
    ResumeQueuedInputRequest, ResumeQueuedInputResult, RuntimeCommand, RuntimeCommandResult,
    RuntimeEventEnvelope, RuntimeHostCapabilities, RuntimeHostHealth, SubmitInputMode, TodoItemId,
    UploadAttachmentResult,
};
use ts_rs::{Config, TS};

const OUTPUT_FILE: &str = "assistant-protocol.ts";

fn export_all(output_directory: PathBuf) -> Result<PathBuf, Box<dyn Error>> {
    fs::create_dir_all(&output_directory)?;
    let config = Config::new()
        .with_out_dir(&output_directory)
        // HTTP JSON 仍传输 number；运行期的水位和计数远低于 JS 安全整数上限。
        .with_large_int("number");

    macro_rules! export_roots {
        ($($type:ty),+ $(,)?) => {
            $(<$type as TS>::export_all(&config)?;)+
        };
    }

    export_roots!(
        RuntimeHostHealth,
        RuntimeHostCapabilities,
        GoalId,
        SubmitInputMode,
        TodoItemId,
        UploadAttachmentResult,
        RuntimeCommand,
        RuntimeCommandResult,
        DeviceGatewayCommand,
        DeviceGatewayCommandResult,
        DeviceGatewayEvent,
        RuntimeEventEnvelope,
        GetApplicationSnapshotRequest,
        GetApplicationSnapshotResult,
        GetSessionViewRequest,
        GetSessionViewResult,
        GetChildTaskViewRequest,
        GetChildTaskViewResult,
        ListConversationPageRequest,
        ListConversationPageResult,
        GetConversationPageAroundRunRequest,
        GetConversationPageAroundRunResult,
        GetToolDetailRequest,
        GetToolDetailResult,
        PrioritizeQueuedInputRequest,
        PrioritizeQueuedInputResult,
        InterruptRunRequest,
        InterruptRunResult,
        ResumeQueuedInputRequest,
        ResumeQueuedInputResult,
        RejectApprovalAndStopRunRequest,
        RejectApprovalAndStopRunResult,
    );

    let output_file = output_directory.join(OUTPUT_FILE);
    normalize_generated_file(&output_file)?;
    Ok(output_file)
}

fn normalize_generated_file(output_file: &PathBuf) -> Result<(), Box<dyn Error>> {
    let generated = fs::read_to_string(output_file)?;
    let normalized = generated
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(output_file, normalized)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let committed_file = manifest_directory
        .join("../../apps/desktop/src/generated")
        .join(OUTPUT_FILE);

    if env::args().any(|argument| argument == "--check") {
        let temporary_directory =
            env::temp_dir().join(format!("ez-assistant-protocol-check-{}", process::id()));
        if temporary_directory.exists() {
            fs::remove_dir_all(&temporary_directory)?;
        }
        let generated_file = export_all(temporary_directory.clone())?;
        let generated = fs::read_to_string(generated_file)?;
        let committed = fs::read_to_string(&committed_file).map_err(|error| {
            format!(
                "cannot read generated binding {}: {error}",
                committed_file.display()
            )
        })?;
        fs::remove_dir_all(temporary_directory)?;
        if generated != committed {
            return Err(format!(
                "{} is stale; run `npm run generate:protocol` in apps/desktop",
                committed_file.display()
            )
            .into());
        }
        return Ok(());
    }

    export_all(
        committed_file
            .parent()
            .expect("generated binding has a parent")
            .to_path_buf(),
    )?;
    Ok(())
}
