//! 构建与运行时共用的中心地址校验；错误不包含输入，以免打印误填的凭据。

pub(crate) fn validate(value: &str) -> Result<(), &'static str> {
    let invalid = "中心地址必须是无凭据、查询参数和 fragment 的 HTTP/HTTPS URL。";
    if value.is_empty()
        || value.chars().any(char::is_whitespace)
        || value.contains('\\')
        || value.split_once("://").is_none_or(|(_, rest)| {
            rest.split('/')
                .next()
                .is_some_and(|authority| authority.contains('@'))
        })
    {
        return Err(invalid);
    }
    let url = url::Url::parse(value).map_err(|_| invalid)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid);
    }
    Ok(())
}
