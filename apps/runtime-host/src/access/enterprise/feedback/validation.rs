//! 无 I/O 输入规范化；中心仍是最终校验权威。
use assistant_protocol::{FeedbackContent, FeedbackFailure as Failure, FeedbackTargetKind};

pub(super) fn text(value: &mut String, limit: usize, required: bool) -> Result<(), Failure> {
    *value = value.trim().to_owned();
    if value.contains('\0') || value.chars().count() > limit || (required && value.is_empty()) {
        return Err(Failure::InvalidInput);
    }
    Ok(())
}
pub(super) fn optional(value: &mut Option<String>, limit: usize) -> Result<(), Failure> {
    if let Some(value) = value {
        text(value, limit, false)?;
    }
    if value.as_ref().is_some_and(String::is_empty) {
        *value = None;
    }
    Ok(())
}
pub(crate) fn content(mut value: FeedbackContent) -> Result<FeedbackContent, Failure> {
    text(&mut value.title, 120, true)?;
    text(&mut value.description, 4000, true)?;
    optional(&mut value.reproduction, 2000)?;
    optional(&mut value.expected_result, 2000)?;
    let target = &mut value.target;
    text(&mut target.name, 120, true)?;
    optional(&mut target.reference, 512)?;
    optional(&mut target.version, 64)?;
    optional(&mut target.content_ref, 512)?;
    optional(&mut target.location, 512)?;
    if target.kind != FeedbackTargetKind::Plugin && target.version.is_some() {
        return Err(Failure::InvalidInput);
    }
    match target.kind {
        FeedbackTargetKind::Platform => {
            if target
                .reference
                .as_ref()
                .is_some_and(|reference| reference != "ez-assistant")
            {
                return Err(Failure::InvalidInput);
            }
            target.reference = Some("ez-assistant".into());
            target.name = "EZ Assistant".into();
        }
        FeedbackTargetKind::Plugin => {
            let reference = target.reference.as_deref().ok_or(Failure::InvalidInput)?;
            if reference.len() > 64
                || reference.split('-').any(|part| {
                    part.is_empty()
                        || !part
                            .bytes()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                })
            {
                return Err(Failure::InvalidInput);
            }
        }
        FeedbackTargetKind::Knowledge => {}
    }
    if serde_json::to_vec(&value)
        .map_err(|_| Failure::InvalidInput)?
        .len()
        > 12 * 1024
    {
        return Err(Failure::TooLarge);
    }
    Ok(value)
}
pub(super) fn id(value: &str) -> Result<(), Failure> {
    if !super::super::client::valid_center_id(value) {
        return Err(Failure::InvalidInput);
    }
    Ok(())
}
