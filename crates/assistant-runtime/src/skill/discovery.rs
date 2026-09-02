//! Skill 文件扫描端口、诊断、名称状态与确定性 Winner 归并。

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    future::Future,
    pin::Pin,
};

use serde::{Deserialize, Serialize};

use super::{SkillMetadata, SkillName, SkillSource};

/// 首版允许进入单个 Session Catalog 的最大有效 Skill 数。
pub const MAX_CATALOG_SKILLS: usize = 256;

/// Host 已完成文件读取、边界校验与 YAML 解析后的有效候选事实。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillCandidate {
    /// 通过格式校验的唯一逻辑名称。
    pub name: SkillName,
    /// 模型目录和产品列表使用的简短说明。
    pub description: String,
    /// 候选项所属固定扫描层级。
    pub source: SkillSource,
    /// Workspace 候选所属根目录顺序；用户级候选为 `None`。
    pub workspace_root_order: Option<usize>,
    /// 供本机诊断和激活后按需访问共享资源使用的候选目录。
    pub source_path: String,
    /// 原始 `SKILL.md` 字节生成的定义身份。
    pub definition_digest: String,
    /// frontmatter 之后的完整 Markdown 指令正文。
    pub body: String,
    /// 与权限无关的兼容元数据。
    pub metadata: SkillMetadata,
    /// 是否允许模型通过稳定 `load_skill` 工具调用。
    pub model_invocable: bool,
    /// 是否允许用户在输入区显式选择。
    pub user_invocable: bool,
}

/// 诊断是否仍允许保留候选项。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDiagnosticSeverity {
    /// 可采用缺省或仅说明覆盖关系，不使整个候选项失效。
    Warning,
    /// 候选项或整次扫描无法安全使用。
    Error,
}

/// Skill 扫描和确定性归并使用的稳定诊断分类。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDiagnosticCode {
    /// 固定 Root 无法完整读取。
    RootUnreadable,
    /// 至少一个 Root 未完成，禁止发布部分 Winner。
    ScanIncomplete,
    /// 本次全部 Root 的候选总数超过上限。
    CandidateLimitExceeded,
    /// 候选目录缺少可读的普通 `SKILL.md`。
    MissingDefinition,
    /// `SKILL.md` 超过上限。
    DefinitionTooLarge,
    /// YAML frontmatter 超过上限。
    FrontmatterTooLarge,
    /// frontmatter 边界、UTF-8 或 YAML 语法无效。
    InvalidFrontmatter,
    /// `name` 或 `description` 缺失或类型错误。
    MissingRequiredField,
    /// `name` 不满足稳定格式约束。
    InvalidName,
    /// `description` 为空或超过长度上限。
    InvalidDescription,
    /// 可选字段类型错误，已采用缺省值。
    OptionalFieldDefaulted,
    /// 出现当前客户端不识别的 frontmatter 字段。
    UnknownField,
    /// 包中包含符号链接或其他不支持的文件类型。
    SpecialFile,
    /// 同一名称在相同来源层级出现多个有效候选。
    SameSourceConflict,
    /// 名称被全局开关禁用。
    DisabledByName,
    /// 候选项被更高优先级的同名来源覆盖。
    Shadowed,
    /// 用户和模型调用资格均被关闭。
    NotInvocable,
    /// 有效 Winner 超过单 Catalog 上限。
    CatalogLimitExceeded,
    /// 旧 Session 没有可恢复的 Catalog 事实。
    LegacyCatalogUnavailable,
}

/// 可定位且可稳定排序的本机发现诊断。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillDiagnostic {
    /// 诊断严重级别。
    pub severity: SkillDiagnosticSeverity,
    /// 供产品层稳定映射中文文案的分类码。
    pub code: SkillDiagnosticCode,
    /// 能够定位到固定 Root 时记录来源层级。
    pub source: Option<SkillSource>,
    /// 只进入本机诊断的源路径，不进入模型 Catalog。
    pub source_path: Option<String>,
    /// 已成功解析名称时记录逻辑名称。
    pub skill_name: Option<SkillName>,
    /// 面向本机诊断的受限补充说明。
    pub detail: String,
}

impl SkillDiagnostic {
    /// 构造尚未绑定具体来源或名称的警告。
    pub fn warning(code: SkillDiagnosticCode, detail: impl Into<String>) -> Self {
        Self {
            severity: SkillDiagnosticSeverity::Warning,
            code,
            source: None,
            source_path: None,
            skill_name: None,
            detail: detail.into(),
        }
    }

    /// 构造尚未绑定具体来源或名称的错误。
    pub fn error(code: SkillDiagnosticCode, detail: impl Into<String>) -> Self {
        Self {
            severity: SkillDiagnosticSeverity::Error,
            code,
            source: None,
            source_path: None,
            skill_name: None,
            detail: detail.into(),
        }
    }
}

/// Host 的一次完整扫描结果。`complete=false` 时禁止消费部分 Winner。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SkillScanResult {
    /// 完成文件边界和 frontmatter 校验的有效候选项。
    pub candidates: Vec<SkillCandidate>,
    /// 有效和无效候选项产生的全部可定位诊断。
    pub diagnostics: Vec<SkillDiagnostic>,
    /// 本次请求的全部 Root 是否完成；为 `false` 时不得选 Winner。
    pub complete: bool,
}

/// Runtime 发给 Host 扫描适配器的文件实现无关请求。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SkillScanRequest {
    /// 当前 Workspace 的规范绝对目录，主目录固定为第 0 项。
    pub workspace_directories: Vec<String>,
}

/// Skill 包扫描的异步返回类型。
pub type SkillScanFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SkillScanResult, SkillPackageSourceError>> + Send + 'a>>;

/// Runtime 使用的 Skill 包发现端口；实现不得在扫描时执行包内脚本。
pub trait SkillPackageSource: Send + Sync {
    /// 扫描请求指定的固定 Root 并返回文件事实；单个坏包应成为诊断而非端口错误。
    fn scan(&self, request: SkillScanRequest) -> SkillScanFuture<'_>;
}

/// 无 Host 嵌入式 Runtime 的稳定空实现。
#[derive(Default)]
pub(crate) struct EmptySkillPackageSource;

impl SkillPackageSource for EmptySkillPackageSource {
    fn scan(&self, _request: SkillScanRequest) -> SkillScanFuture<'_> {
        Box::pin(async {
            Ok(SkillScanResult {
                complete: true,
                ..SkillScanResult::default()
            })
        })
    }
}

/// 扫描任务自身无法完成时返回的基础设施错误。
#[derive(Debug)]
pub struct SkillPackageSourceError {
    message: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl SkillPackageSourceError {
    /// 构造不携带底层实现细节的稳定基础设施错误。
    pub fn new(message: &'static str) -> Self {
        Self {
            message,
            source: None,
        }
    }

    /// 保留进程内 source，同时只向上层暴露稳定安全消息。
    pub fn with_source(message: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            message,
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for SkillPackageSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for SkillPackageSourceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// 全局名称开关的持久事实；没有事实的名称默认启用。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillNameState {
    /// SQLite 唯一键，不拼接来源、路径或 digest。
    pub name: SkillName,
    /// 当前全局开关值。
    pub enabled: bool,
    /// 最近一次用户变更时间。
    pub updated_at_ms: u64,
}

/// 原子修改一个 Skill 名称开关的高层 Store 命令。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillNameStateChange {
    /// 已通过格式校验的唯一名称键。
    pub name: SkillName,
    /// 需要保存的新开关值。
    pub enabled: bool,
    /// 本次用户变更时间。
    pub updated_at_ms: u64,
}

/// 当前发现投影能否用于后续 Catalog 构造。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillDiscoveryStatus {
    /// 本次请求的全部 Root 均扫描完成，可以消费 Winner。
    Available,
    /// 扫描不完整，只可展示候选和诊断，不能消费部分 Winner。
    Unavailable,
}

/// 当前文件投影与名称开关归并后的确定性结果；还不是 Session 冻结 Catalog。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillDiscovery {
    /// 当前投影的整体可用性。
    pub status: SkillDiscoveryStatus,
    /// 全部格式有效的候选项，包括禁用和被覆盖来源。
    pub candidates: Vec<SkillCandidate>,
    /// 应用名称开关、来源优先级和调用资格后的当前生效项。
    pub winners: Vec<SkillCandidate>,
    /// Host 文件诊断与 Runtime 归并诊断的稳定有序集合。
    pub diagnostics: Vec<SkillDiagnostic>,
}

/// 应用名称启停与固定来源优先级，构造当前管理投影。
pub fn compile_skill_discovery(
    mut scan: SkillScanResult,
    states: &[SkillNameState],
) -> SkillDiscovery {
    // 先冻结输入顺序，后续分组和诊断不得依赖文件系统 `read_dir` 顺序。
    scan.candidates.sort_by(candidate_order);
    sort_diagnostics(&mut scan.diagnostics);
    if !scan.complete {
        // Root 读取失败或全局候选超限时，部分候选只保留给设置页诊断。
        scan.diagnostics.push(SkillDiagnostic::error(
            SkillDiagnosticCode::ScanIncomplete,
            "skill roots were not scanned completely",
        ));
        sort_diagnostics(&mut scan.diagnostics);
        return SkillDiscovery {
            status: SkillDiscoveryStatus::Unavailable,
            candidates: scan.candidates,
            winners: Vec::new(),
            diagnostics: scan.diagnostics,
        };
    }

    let enabled = states
        .iter()
        .map(|state| (state.name.clone(), state.enabled))
        .collect::<BTreeMap<_, _>>();
    let mut by_name = BTreeMap::<SkillName, Vec<SkillCandidate>>::new();
    for candidate in &scan.candidates {
        by_name
            .entry(candidate.name.clone())
            .or_default()
            .push(candidate.clone());
    }

    let mut winners = Vec::new();
    for (name, candidates) in by_name {
        // 开关绑定逻辑名称而非当前 Winner；禁用后绝不选择低优先级候补。
        if enabled.get(&name) == Some(&false) {
            scan.diagnostics.push(name_diagnostic(
                SkillDiagnosticSeverity::Warning,
                SkillDiagnosticCode::DisabledByName,
                &name,
                "all candidates with this name are disabled",
            ));
            continue;
        }

        let best_priority = candidates
            .first()
            .map(candidate_priority)
            .expect("grouped candidates are non-empty");
        let same_priority = candidates
            .iter()
            .filter(|candidate| candidate_priority(candidate) == best_priority)
            .collect::<Vec<_>>();
        if same_priority.len() > 1 {
            // 同层没有可证明的稳定胜者，不能拿路径或枚举顺序偷偷裁决。
            scan.diagnostics.push(name_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::SameSourceConflict,
                &name,
                "multiple valid candidates exist at the same source priority",
            ));
            continue;
        }

        let winner = same_priority[0];
        if !winner.model_invocable && !winner.user_invocable {
            scan.diagnostics.push(name_diagnostic(
                SkillDiagnosticSeverity::Warning,
                SkillDiagnosticCode::NotInvocable,
                &name,
                "candidate is unavailable to both user and model invocation",
            ));
            continue;
        }
        for shadowed in candidates
            .iter()
            .filter(|candidate| candidate_priority(candidate) != best_priority)
        {
            let mut diagnostic = name_diagnostic(
                SkillDiagnosticSeverity::Warning,
                SkillDiagnosticCode::Shadowed,
                &name,
                "candidate is shadowed by a higher-priority source",
            );
            diagnostic.source = Some(shadowed.source);
            diagnostic.source_path = Some(shadowed.source_path.clone());
            scan.diagnostics.push(diagnostic);
        }
        winners.push(winner.clone());
    }

    winners.sort_by(|left, right| left.name.cmp(&right.name));
    if winners.len() > MAX_CATALOG_SKILLS {
        // 以名称顺序保留固定前缀，使超限输入仍得到可重复的投影和诊断。
        let excluded = winners.split_off(MAX_CATALOG_SKILLS);
        for candidate in excluded {
            scan.diagnostics.push(name_diagnostic(
                SkillDiagnosticSeverity::Error,
                SkillDiagnosticCode::CatalogLimitExceeded,
                &candidate.name,
                "candidate exceeds the effective catalog skill limit",
            ));
        }
    }
    sort_diagnostics(&mut scan.diagnostics);
    SkillDiscovery {
        status: SkillDiscoveryStatus::Available,
        candidates: scan.candidates,
        winners,
        diagnostics: scan.diagnostics,
    }
}

fn candidate_order(left: &SkillCandidate, right: &SkillCandidate) -> std::cmp::Ordering {
    left.name
        .cmp(&right.name)
        .then(candidate_priority(left).cmp(&candidate_priority(right)))
        .then(left.source_path.cmp(&right.source_path))
        .then(left.definition_digest.cmp(&right.definition_digest))
}

fn candidate_priority(candidate: &SkillCandidate) -> (u8, usize, SkillSource) {
    match candidate.source {
        SkillSource::WorkspaceEzAssistant | SkillSource::WorkspaceAgents => (
            0,
            candidate.workspace_root_order.unwrap_or(usize::MAX),
            candidate.source,
        ),
        SkillSource::UserEzAssistant | SkillSource::UserAgents => (1, 0, candidate.source),
    }
}

fn name_diagnostic(
    severity: SkillDiagnosticSeverity,
    code: SkillDiagnosticCode,
    name: &SkillName,
    detail: &str,
) -> SkillDiagnostic {
    SkillDiagnostic {
        severity,
        code,
        source: None,
        source_path: None,
        skill_name: Some(name.clone()),
        detail: detail.to_owned(),
    }
}

/// 按稳定领域字段排序诊断，禁止把文件系统枚举顺序泄漏到产品投影。
pub fn sort_diagnostics(diagnostics: &mut [SkillDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.severity
            .cmp(&right.severity)
            .then(left.code.cmp(&right.code))
            .then(left.skill_name.cmp(&right.skill_name))
            .then(left.source.cmp(&right.source))
            .then(left.source_path.cmp(&right.source_path))
            .then(left.detail.cmp(&right.detail))
    });
}

/// 返回默认启用之外显式保存的名称集合，供测试和管理投影复用。
pub fn explicit_skill_states(states: &[SkillNameState]) -> BTreeSet<SkillName> {
    states.iter().map(|state| state.name.clone()).collect()
}
