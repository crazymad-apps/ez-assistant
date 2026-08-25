//! Skill 跨发现、Catalog 与 Activation 共用的稳定值类型。

use std::{collections::BTreeMap, error::Error, fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 通过 Agent Skills 格式约束校验的稳定名称。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SkillName(String);

impl SkillName {
    /// 校验并构造名称。名称只允许小写字母、数字和单个连字符。
    pub fn parse(value: impl Into<String>) -> Result<Self, SkillNameError> {
        let value = value.into();
        let bytes = value.as_bytes();
        if bytes.is_empty() {
            return Err(SkillNameError::Empty);
        }
        if bytes.len() > 64 {
            return Err(SkillNameError::TooLong);
        }
        if bytes.first() == Some(&b'-')
            || bytes.last() == Some(&b'-')
            || bytes.windows(2).any(|pair| pair == b"--")
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        {
            return Err(SkillNameError::InvalidFormat);
        }
        Ok(Self(value))
    }

    /// 返回可直接用作 Catalog、工具参数和 SQLite 主键的校验后原值。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SkillName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for SkillName {
    type Err = SkillNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for SkillName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SkillName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// Skill 名称不满足稳定格式约束的原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillNameError {
    /// 名称为空。
    Empty,
    /// 名称超过 64 个 ASCII 字节。
    TooLong,
    /// 名称包含非法字符、首尾连字符或连续连字符。
    InvalidFormat,
}

impl fmt::Display for SkillNameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "skill name must not be empty",
            Self::TooLong => "skill name must not exceed 64 bytes",
            Self::InvalidFormat => "skill name has an invalid format",
        })
    }
}

impl Error for SkillNameError {}

/// 固定扫描来源；枚举顺序就是确定性覆盖优先级。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    /// 当前工作区的 `.ez-assistant/skills`。
    WorkspaceEzAssistant,
    /// 当前工作区的 `.agents/skills`。
    WorkspaceAgents,
    /// 用户 Home 下的 `.ez-assistant/skills`。
    UserEzAssistant,
    /// 用户 Home 下的 `.agents/skills`。
    UserAgents,
}

/// 从通用 frontmatter 保留的兼容元数据；不参与权限决策。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SkillMetadata {
    /// 原包声明的许可证文本。
    pub license: Option<String>,
    /// 原包声明的运行环境兼容性文本。
    pub compatibility: Option<String>,
    /// 只接受字符串键值的扩展元数据。
    pub attributes: BTreeMap<String, String>,
    /// 原包声明的工具提示，仅用于兼容展示，不转换为授权规则。
    pub allowed_tools: Vec<String>,
}
