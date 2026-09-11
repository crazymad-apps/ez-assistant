//! 软件发布版本的规范解析；构建脚本与运行时共用，不包含独立协议代际。

/// 将规范 `major.minor.patch` 解析为可按字典序比较的数值三元组。
/// 每段限制为 u32，拒绝前导零、前后空白、预发布和 build metadata；非法值返回 None。
pub fn parse_software_version(raw: &str) -> Option<[u32; 3]> {
    let mut components = raw.split('.');
    let mut version = [0; 3];
    for component in &mut version {
        let text = components.next()?;
        if text.is_empty()
            || !text.bytes().all(|byte| byte.is_ascii_digit())
            || (text.len() > 1 && text.starts_with('0'))
        {
            return None;
        }
        *component = text.parse().ok()?;
    }
    components.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_versions_are_canonical_bounded_numeric_triples() {
        assert_eq!(parse_software_version("0.25.2"), Some([0, 25, 2]));
        assert!(parse_software_version("0.25.10") > parse_software_version("0.25.2"));
        assert_eq!(
            parse_software_version("4294967295.0.0"),
            Some([u32::MAX, 0, 0])
        );
        for value in [
            "",
            "0.25",
            "0.25.2.1",
            "v0.25.2",
            "00.25.2",
            "0.25.2-rc.1",
            "0.25.2+build",
            "0.25.2 ",
            "+1.0.0",
            "4294967296.0.0",
            "０.25.2",
        ] {
            assert_eq!(parse_software_version(value), None, "{value}");
        }
    }
}
