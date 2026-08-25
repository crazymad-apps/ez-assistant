//! 四个固定本地 Root 的有界 Skill 扫描与 frontmatter 解析。

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use assistant_runtime::{
    SkillCandidate, SkillDiagnostic, SkillDiagnosticCode, SkillDiagnosticSeverity, SkillMetadata,
    SkillName, SkillPackageSource, SkillPackageSourceError, SkillScanFuture, SkillScanRequest,
    SkillScanResult, SkillSource, sort_diagnostics,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

// 所有生产扫描上限集中在 Host 文件适配层；Runtime 和协议不复制物理 I/O 限制。
const MAX_CANDIDATES: usize = 1_024;
const MAX_DEFINITION_BYTES: u64 = 256 * 1024;
const MAX_FRONTMATTER_BYTES: usize = 32 * 1024;
const MAX_DESCRIPTION_CHARS: usize = 1_024;

/// 生产使用固定默认值，测试可缩小单项上限而无需创建超大临时文件。
#[derive(Clone, Copy)]
struct ScanLimits {
    /// 四个 Root 合计允许的直接候选目录数。
    candidates: usize,
    /// 单个 `SKILL.md` 的字节上限。
    definition_bytes: u64,
    /// 单个 YAML frontmatter 的字节上限。
    frontmatter_bytes: usize,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            candidates: MAX_CANDIDATES,
            definition_bytes: MAX_DEFINITION_BYTES,
            frontmatter_bytes: MAX_FRONTMATTER_BYTES,
        }
    }
}

/// 正式 Host 的本地 Skill 包扫描适配器。
pub(super) struct HostSkillPackageSource {
    /// Host 启动时解析的用户 Home；不可用时扫描返回 incomplete 而非伪造路径。
    user_home: Option<PathBuf>,
}

impl HostSkillPackageSource {
    /// 构造扫描适配器；不在此时读取任一 Skill Root。
    pub(super) fn new(user_home: Option<PathBuf>) -> Self {
        Self { user_home }
    }
}

impl SkillPackageSource for HostSkillPackageSource {
    fn scan(&self, request: SkillScanRequest) -> SkillScanFuture<'_> {
        let user_home = self.user_home.clone();
        Box::pin(async move {
            // `std::fs` 和 YAML 解析都在专用阻塞任务中执行，不占用 Runtime 异步执行线程。
            tokio::task::spawn_blocking(move || match user_home {
                Some(user_home) => scan_sync(&user_home, request.workspace_directory.as_deref()),
                None => SkillScanResult {
                    candidates: Vec::new(),
                    diagnostics: vec![SkillDiagnostic::error(
                        SkillDiagnosticCode::RootUnreadable,
                        "user home directory is unavailable",
                    )],
                    complete: false,
                },
            })
            .await
            .map_err(|source| {
                SkillPackageSourceError::with_source(
                    "skill package scan task did not complete",
                    source,
                )
            })
        })
    }
}

fn scan_sync(user_home: &Path, workspace: Option<&str>) -> SkillScanResult {
    scan_sync_with_limits(user_home, workspace, ScanLimits::default())
}

fn scan_sync_with_limits(
    user_home: &Path,
    workspace: Option<&str>,
    limits: ScanLimits,
) -> SkillScanResult {
    // 插入顺序与 Runtime 的 `SkillSource` 排序完全一致；只允许这四个固定 Root。
    let mut roots = Vec::with_capacity(4);
    if let Some(workspace) = workspace {
        let workspace = Path::new(workspace);
        roots.push((
            SkillSource::WorkspaceEzAssistant,
            workspace.join(".ez-assistant/skills"),
        ));
        roots.push((
            SkillSource::WorkspaceAgents,
            workspace.join(".agents/skills"),
        ));
    }
    roots.push((
        SkillSource::UserEzAssistant,
        user_home.join(".ez-assistant/skills"),
    ));
    roots.push((SkillSource::UserAgents, user_home.join(".agents/skills")));

    let mut result = SkillScanResult {
        complete: true,
        ..SkillScanResult::default()
    };
    let mut candidate_count = 0_usize;
    for (source, root) in roots {
        // 全局候选超限或任一 Root 读取不完整后立即停止，禁止发布部分扫描结果。
        if !scan_root(source, &root, limits, &mut candidate_count, &mut result) {
            result.complete = false;
            break;
        }
    }
    result.candidates.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then(left.source_path.cmp(&right.source_path))
            .then(left.name.cmp(&right.name))
    });
    sort_diagnostics(&mut result.diagnostics);
    result
}

fn scan_root(
    source: SkillSource,
    root: &Path,
    limits: ScanLimits,
    candidate_count: &mut usize,
    result: &mut SkillScanResult,
) -> bool {
    if root.parent().is_some_and(|parent| {
        fs::symlink_metadata(parent)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
    }) {
        result.diagnostics.push(path_diagnostic(
            SkillDiagnosticSeverity::Error,
            SkillDiagnosticCode::RootUnreadable,
            source,
            root,
            "skill root parent must not be a symbolic link",
        ));
        return false;
    }
    // 不存在是正常空 Root；存在但不是普通目录或无法完整枚举则令整次扫描 incomplete。
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => {
            result.diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::RootUnreadable,
                source,
                root,
                "skill root could not be inspected",
            ));
            return false;
        }
    };
    if !metadata.file_type().is_dir() {
        result.diagnostics.push(path_diagnostic(
            SkillDiagnosticSeverity::Error,
            SkillDiagnosticCode::RootUnreadable,
            source,
            root,
            "skill root is not an ordinary directory",
        ));
        return false;
    }
    let entries = match sorted_entries(root) {
        Ok(entries) => entries,
        Err(_) => {
            result.diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::RootUnreadable,
                source,
                root,
                "skill root could not be read completely",
            ));
            return false;
        }
    };
    for path in entries {
        // Root 只把直接普通子目录视为候选，普通文件不会被当作独立 Skill 包。
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                result.diagnostics.push(path_diagnostic(
                    SkillDiagnosticSeverity::Error,
                    SkillDiagnosticCode::SpecialFile,
                    source,
                    &path,
                    "candidate entry could not be inspected",
                ));
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            result.diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::SpecialFile,
                source,
                &path,
                "symbolic-link candidates are not supported",
            ));
            continue;
        }
        if !metadata.file_type().is_dir() {
            continue;
        }
        *candidate_count += 1;
        if *candidate_count > limits.candidates {
            result.diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::CandidateLimitExceeded,
                source,
                &path,
                "skill candidate limit was exceeded",
            ));
            return false;
        }
        if let Some(candidate) = scan_candidate(source, &path, limits, &mut result.diagnostics) {
            result.candidates.push(candidate);
        }
    }
    true
}

fn scan_candidate(
    source: SkillSource,
    directory: &Path,
    limits: ScanLimits,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<SkillCandidate> {
    // 先用 symlink_metadata 拒绝链接和特殊文件，再读取受大小限制的定义字节。
    let definition_path = directory.join("SKILL.md");
    let definition_metadata = match fs::symlink_metadata(&definition_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::MissingDefinition,
                source,
                directory,
                "candidate does not contain SKILL.md",
            ));
            return None;
        }
        Err(_) => {
            diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::MissingDefinition,
                source,
                &definition_path,
                "SKILL.md could not be inspected",
            ));
            return None;
        }
    };
    if !definition_metadata.file_type().is_file() {
        diagnostics.push(path_diagnostic(
            SkillDiagnosticSeverity::Error,
            SkillDiagnosticCode::SpecialFile,
            source,
            &definition_path,
            "SKILL.md must be an ordinary file",
        ));
        return None;
    }
    if definition_metadata.len() > limits.definition_bytes {
        diagnostics.push(path_diagnostic(
            SkillDiagnosticSeverity::Error,
            SkillDiagnosticCode::DefinitionTooLarge,
            source,
            &definition_path,
            "SKILL.md exceeds the size limit",
        ));
        return None;
    }
    let definition = match fs::read(&definition_path) {
        Ok(definition) if definition.len() as u64 <= limits.definition_bytes => definition,
        Ok(_) => {
            diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::DefinitionTooLarge,
                source,
                &definition_path,
                "SKILL.md changed beyond the size limit while being read",
            ));
            return None;
        }
        Err(_) => {
            diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::MissingDefinition,
                source,
                &definition_path,
                "SKILL.md could not be read",
            ));
            return None;
        }
    };
    let parsed = parse_definition(
        source,
        &definition_path,
        &definition,
        limits.frontmatter_bytes,
        diagnostics,
    )?;
    let source_path = match directory.to_str() {
        Some(path) => path.to_owned(),
        None => {
            diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::SpecialFile,
                source,
                directory,
                "candidate path is not valid UTF-8",
            ));
            return None;
        }
    };

    // 候选身份只绑定 `SKILL.md`；普通资源留在共享目录，由既有工具按需访问。
    let mut hasher = Sha256::new();
    hasher.update(b"skill-definition-v1\0");
    hasher.update(&definition);
    Some(SkillCandidate {
        name: parsed.name,
        description: parsed.description,
        source,
        source_path,
        definition_digest: format!("sha256-v1:{:x}", hasher.finalize()),
        body: parsed.body,
        metadata: parsed.metadata,
        model_invocable: parsed.model_invocable,
        user_invocable: parsed.user_invocable,
    })
}

struct ParsedDefinition {
    name: SkillName,
    description: String,
    body: String,
    metadata: SkillMetadata,
    model_invocable: bool,
    user_invocable: bool,
}

fn parse_definition(
    source: SkillSource,
    path: &Path,
    bytes: &[u8],
    frontmatter_limit: usize,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<ParsedDefinition> {
    // frontmatter 边界先按字节对应的 Markdown 行规则定位，YAML crate 只负责反序列化块内容。
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::InvalidFrontmatter,
                source,
                path,
                "SKILL.md must be UTF-8",
            ));
            return None;
        }
    };
    let (frontmatter, body) = match split_frontmatter(text) {
        Some(parts) => parts,
        None => {
            diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::InvalidFrontmatter,
                source,
                path,
                "SKILL.md must start with a closed YAML frontmatter block",
            ));
            return None;
        }
    };
    if frontmatter.len() > frontmatter_limit {
        diagnostics.push(path_diagnostic(
            SkillDiagnosticSeverity::Error,
            SkillDiagnosticCode::FrontmatterTooLarge,
            source,
            path,
            "SKILL.md frontmatter exceeds the size limit",
        ));
        return None;
    }
    let fields = match serde_saphyr::from_str::<BTreeMap<String, Value>>(frontmatter) {
        Ok(fields) => fields,
        Err(_) => {
            diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::InvalidFrontmatter,
                source,
                path,
                "SKILL.md frontmatter is not valid YAML",
            ));
            return None;
        }
    };
    let raw_name = required_string(&fields, "name", source, path, diagnostics)?;
    let name = match SkillName::parse(raw_name) {
        Ok(name) => name,
        Err(_) => {
            diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::InvalidName,
                source,
                path,
                "skill name does not satisfy the format constraints",
            ));
            return None;
        }
    };
    let description = required_string(&fields, "description", source, path, diagnostics)?;
    if description.trim().is_empty() || description.chars().count() > MAX_DESCRIPTION_CHARS {
        diagnostics.push(named_path_diagnostic(
            SkillDiagnosticSeverity::Error,
            SkillDiagnosticCode::InvalidDescription,
            source,
            path,
            &name,
            "skill description is empty or too large",
        ));
        return None;
    }
    let license = optional_string(&fields, "license", source, path, &name, diagnostics);
    let compatibility = optional_string(&fields, "compatibility", source, path, &name, diagnostics);
    let attributes = optional_string_map(&fields, source, path, &name, diagnostics);
    let allowed_tools = optional_allowed_tools(&fields, source, path, &name, diagnostics);
    let disable_model = optional_bool(
        &fields,
        "disable-model-invocation",
        false,
        source,
        path,
        &name,
        diagnostics,
    );
    let user_invocable = optional_bool(
        &fields,
        "user-invocable",
        true,
        source,
        path,
        &name,
        diagnostics,
    );
    let known = BTreeSet::from([
        "name",
        "description",
        "license",
        "compatibility",
        "metadata",
        "allowed-tools",
        "disable-model-invocation",
        "user-invocable",
    ]);
    for field in fields
        .keys()
        .filter(|field| !known.contains(field.as_str()))
    {
        diagnostics.push(named_path_diagnostic(
            SkillDiagnosticSeverity::Warning,
            SkillDiagnosticCode::UnknownField,
            source,
            path,
            &name,
            format!("unknown frontmatter field: {field}"),
        ));
    }
    Some(ParsedDefinition {
        name,
        description,
        body: body.to_owned(),
        metadata: SkillMetadata {
            license,
            compatibility,
            attributes,
            allowed_tools,
        },
        model_invocable: !disable_model,
        user_invocable,
    })
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    // 只接受文件首行 `---`，并兼容 LF/CRLF；不猜测或修复缺失的闭合边界。
    let first_end = text.find('\n').unwrap_or(text.len());
    if text[..first_end].trim_end_matches('\r') != "---" || first_end == text.len() {
        return None;
    }
    let frontmatter_start = first_end + 1;
    let mut offset = frontmatter_start;
    for line in text[frontmatter_start..].split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if content == "---" {
            let body_start = offset + line.len();
            return Some((&text[frontmatter_start..offset], &text[body_start..]));
        }
        offset += line.len();
    }
    let final_line = &text[offset..];
    (final_line.trim_end_matches('\r') == "---").then_some((&text[frontmatter_start..offset], ""))
}

fn required_string(
    fields: &BTreeMap<String, Value>,
    field: &str,
    source: SkillSource,
    path: &Path,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<String> {
    match fields.get(field).and_then(Value::as_str) {
        Some(value) => Some(value.to_owned()),
        None => {
            diagnostics.push(path_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::MissingRequiredField,
                source,
                path,
                format!("required frontmatter field is missing or invalid: {field}"),
            ));
            None
        }
    }
}

fn optional_string(
    fields: &BTreeMap<String, Value>,
    field: &str,
    source: SkillSource,
    path: &Path,
    name: &SkillName,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<String> {
    let value = fields.get(field)?;
    if let Some(value) = value.as_str() {
        return Some(value.to_owned());
    }
    diagnostics.push(named_path_diagnostic(
        SkillDiagnosticSeverity::Warning,
        SkillDiagnosticCode::OptionalFieldDefaulted,
        source,
        path,
        name,
        format!("optional frontmatter field was ignored: {field}"),
    ));
    None
}

fn optional_bool(
    fields: &BTreeMap<String, Value>,
    field: &str,
    default: bool,
    source: SkillSource,
    path: &Path,
    name: &SkillName,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> bool {
    let Some(value) = fields.get(field) else {
        return default;
    };
    if let Some(value) = value.as_bool() {
        return value;
    }
    diagnostics.push(named_path_diagnostic(
        SkillDiagnosticSeverity::Warning,
        SkillDiagnosticCode::OptionalFieldDefaulted,
        source,
        path,
        name,
        format!("optional frontmatter field was defaulted: {field}"),
    ));
    default
}

fn optional_string_map(
    fields: &BTreeMap<String, Value>,
    source: SkillSource,
    path: &Path,
    name: &SkillName,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> BTreeMap<String, String> {
    let Some(value) = fields.get("metadata") else {
        return BTreeMap::new();
    };
    let Some(object) = value.as_object() else {
        optional_default_warning("metadata", source, path, name, diagnostics);
        return BTreeMap::new();
    };
    let mut result = BTreeMap::new();
    for (key, value) in object {
        let Some(value) = value.as_str() else {
            optional_default_warning("metadata", source, path, name, diagnostics);
            return BTreeMap::new();
        };
        result.insert(key.clone(), value.to_owned());
    }
    result
}

fn optional_allowed_tools(
    fields: &BTreeMap<String, Value>,
    source: SkillSource,
    path: &Path,
    name: &SkillName,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Vec<String> {
    let Some(value) = fields.get("allowed-tools") else {
        return Vec::new();
    };
    if let Some(value) = value.as_str() {
        return value.split_whitespace().map(str::to_owned).collect();
    }
    if let Some(values) = value.as_array()
        && values.iter().all(Value::is_string)
    {
        return values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
    }
    optional_default_warning("allowed-tools", source, path, name, diagnostics);
    Vec::new()
}

fn optional_default_warning(
    field: &str,
    source: SkillSource,
    path: &Path,
    name: &SkillName,
    diagnostics: &mut Vec<SkillDiagnostic>,
) {
    diagnostics.push(named_path_diagnostic(
        SkillDiagnosticSeverity::Warning,
        SkillDiagnosticCode::OptionalFieldDefaulted,
        source,
        path,
        name,
        format!("optional frontmatter field was defaulted: {field}"),
    ));
}

fn sorted_entries(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by(|left, right| path_order(left, right));
    Ok(entries)
}

fn path_order(left: &Path, right: &Path) -> std::cmp::Ordering {
    left.as_os_str()
        .to_string_lossy()
        .cmp(&right.as_os_str().to_string_lossy())
}

fn path_diagnostic(
    severity: SkillDiagnosticSeverity,
    code: SkillDiagnosticCode,
    source: SkillSource,
    path: &Path,
    detail: impl Into<String>,
) -> SkillDiagnostic {
    SkillDiagnostic {
        severity,
        code,
        source: Some(source),
        source_path: Some(path.to_string_lossy().into_owned()),
        skill_name: None,
        detail: detail.into(),
    }
}

fn named_path_diagnostic(
    severity: SkillDiagnosticSeverity,
    code: SkillDiagnosticCode,
    source: SkillSource,
    path: &Path,
    name: &SkillName,
    detail: impl Into<String>,
) -> SkillDiagnostic {
    let mut diagnostic = path_diagnostic(severity, code, source, path, detail);
    diagnostic.skill_name = Some(name.clone());
    diagnostic
}

#[cfg(test)]
mod tests {
    use assistant_runtime::{SkillDiagnosticCode, compile_skill_discovery};
    use tempfile::TempDir;

    use super::*;

    fn write_skill(root: &Path, directory: &str, name: &str, extra: &str) -> PathBuf {
        let directory = root.join(directory);
        fs::create_dir_all(&directory).expect("skill directory");
        fs::write(
            directory.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {name} description\n{extra}---\n\n{name} body\n"
            ),
        )
        .expect("skill definition");
        directory
    }

    #[test]
    fn four_roots_use_the_confirmed_priority_and_missing_roots_are_empty() {
        let temporary = TempDir::new().expect("tempdir");
        let home = temporary.path().join("home");
        let workspace = temporary.path().join("workspace");
        let roots = [
            workspace.join(".ez-assistant/skills"),
            workspace.join(".agents/skills"),
            home.join(".ez-assistant/skills"),
            home.join(".agents/skills"),
        ];
        for (index, root) in roots.iter().enumerate() {
            write_skill(root, &format!("review-{index}"), "review", "");
        }
        let scan = scan_sync(&home, Some(workspace.to_str().expect("UTF-8 workspace")));
        assert!(scan.complete);
        assert_eq!(scan.candidates.len(), 4);
        let discovery = compile_skill_discovery(scan, &[]);
        assert_eq!(discovery.winners.len(), 1);
        assert_eq!(
            discovery.winners[0].source,
            SkillSource::WorkspaceEzAssistant
        );

        let empty = scan_sync(temporary.path(), None);
        assert!(empty.complete);
        assert!(empty.candidates.is_empty());
        assert!(empty.diagnostics.is_empty());
    }

    #[test]
    fn same_source_conflicts_optional_defaults_and_bad_yaml_are_diagnosed() {
        let temporary = TempDir::new().expect("tempdir");
        let root = temporary.path().join(".agents/skills");
        write_skill(&root, "one", "review", "license: 42\nunknown-key: true\n");
        write_skill(&root, "two", "review", "");
        let invalid = root.join("invalid");
        fs::create_dir_all(&invalid).expect("invalid directory");
        fs::write(
            invalid.join("SKILL.md"),
            "---\nname: [broken\ndescription: no\n---\n",
        )
        .expect("invalid definition");

        let discovery = compile_skill_discovery(scan_sync(temporary.path(), None), &[]);
        assert!(discovery.winners.is_empty());
        for expected in [
            SkillDiagnosticCode::OptionalFieldDefaulted,
            SkillDiagnosticCode::UnknownField,
            SkillDiagnosticCode::InvalidFrontmatter,
            SkillDiagnosticCode::SameSourceConflict,
        ] {
            assert!(
                discovery
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == expected),
                "missing {expected:?}"
            );
        }
    }

    #[test]
    fn unreadable_root_makes_the_scan_incomplete_without_partial_winners() {
        let temporary = TempDir::new().expect("tempdir");
        let workspace = temporary.path().join("workspace");
        let valid_root = workspace.join(".ez-assistant/skills");
        write_skill(&valid_root, "review", "review", "");
        let invalid_root = workspace.join(".agents/skills");
        fs::create_dir_all(invalid_root.parent().expect("parent")).expect("agents parent");
        fs::write(&invalid_root, b"not a directory").expect("invalid root");

        let scan = scan_sync(
            temporary.path(),
            Some(workspace.to_str().expect("workspace")),
        );
        assert!(!scan.complete);
        let discovery = compile_skill_discovery(scan, &[]);
        assert!(discovery.winners.is_empty());
        assert!(
            discovery
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == SkillDiagnosticCode::RootUnreadable })
        );
    }

    #[cfg(unix)]
    #[test]
    fn linked_candidates_and_each_discovery_limit_are_rejected() {
        use std::os::unix::fs::symlink;

        let temporary = TempDir::new().expect("tempdir");
        let root = temporary.path().join(".agents/skills");
        fs::create_dir_all(&root).expect("skill root");
        let target_root = temporary.path().join("targets");
        let target = write_skill(&target_root, "linked", "linked", "");
        symlink(target, root.join("linked")).expect("candidate symlink");
        let scan = scan_sync(temporary.path(), None);
        assert!(scan.candidates.is_empty());
        assert!(
            scan.diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == SkillDiagnosticCode::SpecialFile })
        );

        let cases = [
            (
                ScanLimits {
                    definition_bytes: 8,
                    ..ScanLimits::default()
                },
                SkillDiagnosticCode::DefinitionTooLarge,
            ),
            (
                ScanLimits {
                    frontmatter_bytes: 8,
                    ..ScanLimits::default()
                },
                SkillDiagnosticCode::FrontmatterTooLarge,
            ),
        ];
        for (index, (limits, code)) in cases.into_iter().enumerate() {
            let home = temporary.path().join(format!("limit-{index}"));
            let root = home.join(".agents/skills");
            write_skill(&root, "limited", "limited", "");
            let scan = scan_sync_with_limits(&home, None, limits);
            assert!(scan.candidates.is_empty(), "{code:?}");
            assert!(
                scan.diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "missing {code:?}"
            );
        }

        let home = temporary.path().join("candidate-limit");
        let root = home.join(".agents/skills");
        write_skill(&root, "one", "one", "");
        write_skill(&root, "two", "two", "");
        let scan = scan_sync_with_limits(
            &home,
            None,
            ScanLimits {
                candidates: 1,
                ..ScanLimits::default()
            },
        );
        assert!(!scan.complete);
        assert!(
            scan.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == SkillDiagnosticCode::CandidateLimitExceeded
            })
        );
    }

    #[test]
    fn discovery_keeps_only_the_source_directory_and_does_not_read_resources() {
        let temporary = TempDir::new().expect("tempdir");
        let root = temporary.path().join(".agents/skills");
        let skill = write_skill(&root, "stable", "stable", "");
        let first = scan_sync(temporary.path(), None);
        fs::create_dir_all(skill.join("references/nested")).expect("resource directory");
        fs::write(
            skill.join("references/nested/large.bin"),
            vec![7_u8; 64 * 1024],
        )
        .expect("resource bytes");
        let second = scan_sync(temporary.path(), None);
        assert_eq!(first, second);
        assert_eq!(
            first.candidates[0].source_path,
            skill.to_str().expect("UTF-8 source directory")
        );
    }
}
