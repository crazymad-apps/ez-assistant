//! 原个人域的历史附件定位。调用方先从本域数据库核实附件身份，再生成唯一旧路径。

use assistant_runtime::StoredAttachment;

pub(super) fn current_recorded_resource_path(
    user_home: &Path,
    path: &str,
    environment: &assistant_runtime::SessionExecutionEnvironment,
) -> String {
    let mapped = (|| {
        if user_home.file_name()? != "_personal" || user_home.parent()?.file_name()? != "users" {
            return None;
        }
        let paths = crate::host_layout::paths::Relocation {
            source: user_home.parent()?.parent()?.to_owned(),
            target: user_home.to_owned(),
            roots: vec!["data".into()],
        };
        let current = paths.path(Path::new(path));
        let owned = [
            Some(environment.session_private_directory.as_str()),
            Some(environment.session_attachment_directory.as_str()),
            Some(environment.session_tool_image_directory.as_str()),
            environment.workspace_private_directory.as_deref(),
        ];
        owned
            .into_iter()
            .flatten()
            .any(|root| current.starts_with(root))
            .then(|| current.to_string_lossy().into_owned())
    })();
    mapped.unwrap_or_else(|| path.to_owned())
}
use std::path::Path;

pub(super) fn historical_attachment_path(
    user_home: &Path,
    attachment: &StoredAttachment,
) -> Option<String> {
    if user_home.file_name()? != "_personal" || user_home.parent()?.file_name()? != "users" {
        return None;
    }
    let host = user_home.parent()?.parent()?;
    let expected = super::attachment_io::stable_view_path(
        &user_home
            .join("data/sessions")
            .join(attachment.session_id.as_str())
            .join("attachments"),
        &attachment.attachment_id,
        &attachment.original_name,
    );
    if Path::new(&attachment.agent_readable_path) != expected {
        return None;
    }
    let relative = expected.strip_prefix(user_home).ok()?;
    host.join(relative).to_str().map(str::to_owned)
}
