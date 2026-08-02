//! Shell 子进程的环境变量过滤。

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
};

/// 子进程环境变量的装配策略。
///
/// 规则顺序固定为：先判断 `allow_exact`，未允许的名称再经过 deny
/// 规则，最后应用 `overrides`。因此同一名称同时命中 allow/deny 时
/// allow 优先，override 又可以最终写入或删除它。
///
/// Adapter 不内置 Provider 名单；默认策略按通用 credential 后缀过滤。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentPolicy {
    /// 总是允许传递的精确变量名，优先于 deny 规则。
    pub allow_exact: BTreeSet<OsString>,
    /// 禁止传递的精确变量名。
    pub deny_exact: BTreeSet<OsString>,
    /// 禁止传递的变量名后缀，例如 `_API_KEY`。
    pub deny_suffixes: Vec<OsString>,
    /// 最后生效的显式覆盖；`Some` 写入，`None` 删除。
    pub overrides: BTreeMap<OsString, Option<OsString>>,
}

impl Default for EnvironmentPolicy {
    fn default() -> Self {
        Self {
            allow_exact: BTreeSet::new(),
            deny_exact: BTreeSet::new(),
            deny_suffixes: ["_API_KEY", "_TOKEN", "_SECRET"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            overrides: BTreeMap::new(),
        }
    }
}

impl EnvironmentPolicy {
    /// 显式选择完整继承父进程环境，不应用默认敏感后缀过滤。
    pub fn inherit_all() -> Self {
        Self {
            allow_exact: BTreeSet::new(),
            deny_exact: BTreeSet::new(),
            deny_suffixes: Vec::new(),
            overrides: BTreeMap::new(),
        }
    }

    /// 从当前进程环境构建将注入 Shell 子进程的最终环境。
    pub(crate) fn resolve_current(&self) -> BTreeMap<OsString, OsString> {
        self.resolve(std::env::vars_os())
    }

    /// 对给定父环境应用规则。分离输入便于不修改真实进程环境的确定性测试。
    fn resolve(
        &self,
        parent: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> BTreeMap<OsString, OsString> {
        let mut resolved = parent
            .into_iter()
            .filter(|(name, _)| self.is_allowed(name))
            .collect::<BTreeMap<_, _>>();

        for (name, value) in &self.overrides {
            let equivalent_names = resolved
                .keys()
                .filter(|existing| names_equal(existing, name))
                .cloned()
                .collect::<Vec<_>>();
            for existing in equivalent_names {
                resolved.remove(&existing);
            }
            if let Some(value) = value {
                resolved.insert(name.clone(), value.clone());
            }
        }
        resolved
    }

    fn is_allowed(&self, name: &OsStr) -> bool {
        if self
            .allow_exact
            .iter()
            .any(|allowed| names_equal(name, allowed))
        {
            return true;
        }
        !self
            .deny_exact
            .iter()
            .any(|denied| names_equal(name, denied))
            && !self
                .deny_suffixes
                .iter()
                .any(|suffix| os_str_ends_with(name, suffix))
    }
}

fn os_str_ends_with(value: &OsStr, suffix: &OsStr) -> bool {
    #[cfg(windows)]
    {
        let value = value.as_encoded_bytes();
        let suffix = suffix.as_encoded_bytes();
        return value.len() >= suffix.len()
            && value[value.len() - suffix.len()..].eq_ignore_ascii_case(suffix);
    }

    #[cfg(not(windows))]
    value
        .as_encoded_bytes()
        .ends_with(suffix.as_encoded_bytes())
}

fn names_equal(left: &OsStr, right: &OsStr) -> bool {
    #[cfg(windows)]
    {
        return left
            .as_encoded_bytes()
            .eq_ignore_ascii_case(right.as_encoded_bytes());
    }

    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(name: &str, value: &str) -> (OsString, OsString) {
        (OsString::from(name), OsString::from(value))
    }

    #[test]
    fn allow_has_priority_and_overrides_apply_last() {
        let policy = EnvironmentPolicy {
            allow_exact: [OsString::from("KEEP_TOKEN")].into_iter().collect(),
            deny_exact: [OsString::from("DENIED"), OsString::from("KEEP_TOKEN")]
                .into_iter()
                .collect(),
            deny_suffixes: vec![OsString::from("_TOKEN"), OsString::from("_SECRET")],
            overrides: [
                (OsString::from("DENIED"), Some(OsString::from("restored"))),
                (OsString::from("REMOVE"), None),
                (OsString::from("ADDED"), Some(OsString::from("new"))),
            ]
            .into_iter()
            .collect(),
        };

        let resolved = policy.resolve([
            pair("VISIBLE", "yes"),
            pair("DENIED", "old"),
            pair("OTHER_TOKEN", "secret"),
            pair("KEEP_TOKEN", "allowed"),
            pair("REMOVE", "gone"),
        ]);

        assert_eq!(
            resolved.get(OsStr::new("VISIBLE")),
            Some(&OsString::from("yes"))
        );
        assert_eq!(
            resolved.get(OsStr::new("KEEP_TOKEN")),
            Some(&OsString::from("allowed"))
        );
        assert_eq!(
            resolved.get(OsStr::new("DENIED")),
            Some(&OsString::from("restored"))
        );
        assert_eq!(
            resolved.get(OsStr::new("ADDED")),
            Some(&OsString::from("new"))
        );
        assert!(!resolved.contains_key(OsStr::new("OTHER_TOKEN")));
        assert!(!resolved.contains_key(OsStr::new("REMOVE")));
    }

    #[test]
    fn default_filters_generic_credentials_and_inherit_all_is_explicit() {
        let resolved = EnvironmentPolicy::default().resolve([
            pair("PATH", "/bin"),
            pair("EXAMPLE", "value"),
            pair("SERVICE_API_KEY", "secret"),
            pair("SESSION_TOKEN", "secret"),
            pair("CLIENT_SECRET", "secret"),
        ]);

        assert_eq!(resolved.len(), 2);
        assert_eq!(
            resolved.get(OsStr::new("EXAMPLE")),
            Some(&OsString::from("value"))
        );

        let inherited = EnvironmentPolicy::inherit_all()
            .resolve([pair("SERVICE_API_KEY", "secret"), pair("PATH", "/bin")]);
        assert_eq!(inherited.len(), 2);
        assert!(inherited.contains_key(OsStr::new("SERVICE_API_KEY")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_names_are_ascii_case_insensitive() {
        let policy = EnvironmentPolicy {
            allow_exact: [OsString::from("Keep_Token")].into_iter().collect(),
            deny_exact: [OsString::from("Blocked")].into_iter().collect(),
            overrides: [(OsString::from("Path"), Some(OsString::from("new")))]
                .into_iter()
                .collect(),
            ..EnvironmentPolicy::default()
        };
        let resolved = policy.resolve([
            pair("KEEP_TOKEN", "allowed"),
            pair("BLOCKED", "denied"),
            pair("PATH", "old"),
            pair("other_secret", "denied"),
        ]);

        assert_eq!(
            resolved.get(OsStr::new("KEEP_TOKEN")),
            Some(&OsString::from("allowed"))
        );
        assert!(!resolved.contains_key(OsStr::new("BLOCKED")));
        assert!(!resolved.contains_key(OsStr::new("PATH")));
        assert_eq!(
            resolved.get(OsStr::new("Path")),
            Some(&OsString::from("new"))
        );
        assert!(!resolved.contains_key(OsStr::new("other_secret")));
    }
}
