//! 两种 OpenAI-compatible 协议共用的结构化引用文本投影。

use agent_types::QuotedTextPart;

/// 把规范引用渲染为稳定、转义后的文本块。
///
/// 本地 identity、来源 locator 与展示元数据都不会进入模型请求。
pub(crate) fn render_quoted_text(part: &QuotedTextPart) -> String {
    let mut xml = String::from("<quoted_text>\n  <prefix>");
    push_xml_text(&mut xml, &part.prefix);
    xml.push_str("</prefix>\n  <content format=\"text\">");
    push_xml_text(&mut xml, &part.exact);
    xml.push_str("</content>\n  <suffix>");
    push_xml_text(&mut xml, &part.suffix);
    xml.push_str("</suffix>\n</quoted_text>");
    xml
}

fn push_xml_text(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            _ => output.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_types::{
        MessageId, PartId, QuotedTextPart, QuotedTextSourceOwner, QuotedTextSourceRole,
    };

    use super::*;

    #[test]
    fn renderer_escapes_all_text_and_excludes_local_identity() {
        let rendered = render_quoted_text(&QuotedTextPart {
            quote_id: PartId::new("private-quote-id").expect("valid quote id"),
            exact: "<&>\"'".to_owned(),
            prefix: "prefix <unsafe>".to_owned(),
            suffix: "suffix & tail".to_owned(),
            source_owner: QuotedTextSourceOwner::MainSession {
                session_id: "private-session-id".to_owned(),
            },
            source_generation: 7,
            source_message_id: MessageId::new("private-message-id").expect("valid message id"),
            text_start_utf16: 2,
            text_end_utf16: 7,
            source_role: QuotedTextSourceRole::User,
            source_label: "A & B".to_owned(),
            source_created_at_ms: Some(42),
            source_available: true,
        });

        assert!(rendered.contains("<content format=\"text\">&lt;&amp;&gt;&quot;&apos;</content>"));
        assert!(rendered.contains("<prefix>prefix &lt;unsafe&gt;</prefix>"));
        assert!(!rendered.contains("A &amp; B"));
        assert!(!rendered.contains("private-session-id"));
        assert!(!rendered.contains("private-message-id"));
        assert!(!rendered.contains("private-quote-id"));
    }
}
