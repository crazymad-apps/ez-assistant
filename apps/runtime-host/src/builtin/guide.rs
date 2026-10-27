//! 官网同源正文编入 Host；虚拟引用只命中静态主题，不访问磁盘或网络。

use assistant_runtime::{
    BUILTIN_GUIDE_REFERENCES, BUILTIN_GUIDE_SKILL_NAME, MAX_SKILL_REFERENCE_BYTES,
    SkillPackageSourceError, SkillReferenceRequest,
};

pub(super) const DEFINITION: &str =
    include_str!("../../resources/builtin/skills/ez-assistant-guide/SKILL.md");

const PAGES: [(&str, &str); 4] = [
    (
        BUILTIN_GUIDE_REFERENCES[0],
        include_str!("../../../../docs/site/manual/runtime-environment.md"),
    ),
    (
        BUILTIN_GUIDE_REFERENCES[1],
        include_str!("../../../../docs/site/manual/general-assistant.md"),
    ),
    (
        BUILTIN_GUIDE_REFERENCES[2],
        include_str!("../../../../docs/site/manual/mcp-configuration.md"),
    ),
    (
        BUILTIN_GUIDE_REFERENCES[3],
        include_str!("../../../../docs/site/manual/plugin-development.md"),
    ),
];

// 对原始源字节施加略严格的编译期上限，禁止把过长主题以截断正文交付。
const _: () = {
    let mut index = 0;
    let mut total = 0;
    while index < PAGES.len() {
        assert!(PAGES[index].1.len() <= MAX_SKILL_REFERENCE_BYTES);
        total += PAGES[index].1.len();
        index += 1;
    }
    assert!(total <= MAX_SKILL_REFERENCE_BYTES * 4);
};

pub(crate) fn read_reference(
    request: &SkillReferenceRequest,
) -> Result<Option<&'static str>, SkillPackageSourceError> {
    if request.name.as_str() != BUILTIN_GUIDE_SKILL_NAME {
        return Ok(None);
    }
    let definition = crate::resources::embedded_skill(BUILTIN_GUIDE_SKILL_NAME, DEFINITION)
        .candidates
        .into_iter()
        .next()
        .ok_or_else(|| SkillPackageSourceError::new("embedded guide definition is invalid"))?;
    if request.definition_digest != definition.definition_digest {
        return Ok(None);
    }
    let Some((_, source)) = PAGES.iter().find(|(key, _)| *key == request.reference) else {
        return Ok(None);
    };
    let (_, body) = source
        .strip_prefix("---\n")
        .and_then(|text| text.split_once("\n---\n"))
        .ok_or_else(|| SkillPackageSourceError::new("embedded guide page metadata is invalid"))?;
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use assistant_runtime::SkillName;

    #[test]
    fn embedded_topics_are_exact_source_bodies_with_frozen_identity() {
        let scan = crate::resources::embedded_skill(BUILTIN_GUIDE_SKILL_NAME, DEFINITION);
        assert!(scan.complete);
        let definition = scan.candidates.first().expect("guide definition");
        assert!(definition.user_invocable && definition.model_invocable);
        for (reference, source) in PAGES {
            assert!(
                DEFINITION.contains(reference),
                "each offline topic is navigable"
            );
            let mut request = SkillReferenceRequest {
                name: definition.name.clone(),
                definition_digest: definition.definition_digest.clone(),
                reference: reference.to_owned(),
            };
            let body = read_reference(&request)
                .unwrap()
                .expect("actual embedded topic");
            assert_eq!(body, source.split_once("\n---\n").unwrap().1);
            assert!(!body.trim().is_empty());
            assert!(body.len() <= MAX_SKILL_REFERENCE_BYTES);
            request.definition_digest = "stale".to_owned();
            assert_eq!(read_reference(&request).unwrap(), None);
            request.definition_digest = definition.definition_digest.clone();
            request.name = SkillName::parse("another-skill".to_owned()).unwrap();
            assert_eq!(read_reference(&request).unwrap(), None);
        }
    }

    #[test]
    fn references_never_fall_back_to_arbitrary_files_or_urls() {
        let definition = crate::resources::embedded_skill(BUILTIN_GUIDE_SKILL_NAME, DEFINITION)
            .candidates
            .remove(0);
        for reference in [
            "",
            "references/missing.md",
            "../SKILL.md",
            "/etc/passwd",
            "C:\\data",
            "https://example.invalid",
            "references\\mcp.md",
            "references/mcp.md\0",
        ] {
            assert_eq!(
                read_reference(&SkillReferenceRequest {
                    name: definition.name.clone(),
                    definition_digest: definition.definition_digest.clone(),
                    reference: reference.to_owned(),
                })
                .unwrap(),
                None
            );
        }
    }

    #[test]
    fn plugin_example_uses_the_real_host_manifest_parser() {
        let page = PAGES[3].1;
        let example = page
            .split_once("```json\n")
            .expect("manifest example")
            .1
            .split_once("\n```")
            .expect("closed manifest example")
            .0;
        let manifest = crate::plugins::PluginManifest::parse(example.as_bytes())
            .expect("the shipped example follows the actual plugin contract");
        assert_eq!(manifest.name, "example-tools");
        assert_eq!(manifest.skills, ["example-guide"]);
        assert!(manifest.mcp.is_empty());
        assert!(manifest.configuration.is_empty());
    }
}
