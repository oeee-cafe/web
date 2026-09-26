use markdown::{mdast, to_html, to_mdast, ParseOptions};

/// Process markdown content by converting headings to paragraphs and rendering to HTML
pub fn process_markdown_content(value: &str) -> String {
    let ast = match to_mdast(value, &ParseOptions::default()) {
        Ok(ast) => ast,
        // Templates print what this returns with `|safe`, so text that could
        // not be parsed goes back escaped, never as it came. With the default
        // options a parse only fails where MDX is on, which it is not; this
        // is so that turning it on cannot make every post a way in.
        Err(_) => return escape_html(value),
    };

    let processed_ast = convert_headings_to_paragraphs(ast);

    // Convert AST back to markdown string, then to HTML
    let processed_md = mdast_to_markdown(&processed_ast);
    to_html(&processed_md)
}

fn convert_headings_to_paragraphs(node: mdast::Node) -> mdast::Node {
    match node {
        mdast::Node::Heading(heading) => mdast::Node::Paragraph(mdast::Paragraph {
            children: heading.children,
            position: heading.position,
        }),
        mdast::Node::Root(mut root) => {
            root.children = root
                .children
                .into_iter()
                .map(convert_headings_to_paragraphs)
                .collect();
            mdast::Node::Root(root)
        }
        mdast::Node::Blockquote(mut blockquote) => {
            blockquote.children = blockquote
                .children
                .into_iter()
                .map(convert_headings_to_paragraphs)
                .collect();
            mdast::Node::Blockquote(blockquote)
        }
        mdast::Node::List(mut list) => {
            list.children = list
                .children
                .into_iter()
                .map(convert_headings_to_paragraphs)
                .collect();
            mdast::Node::List(list)
        }
        mdast::Node::ListItem(mut list_item) => {
            list_item.children = list_item
                .children
                .into_iter()
                .map(convert_headings_to_paragraphs)
                .collect();
            mdast::Node::ListItem(list_item)
        }
        _ => node, // Leave other nodes unchanged
    }
}

fn mdast_to_markdown(node: &mdast::Node) -> String {
    // Simple converter - for a full implementation, we'd need the markdown-to-mdast crate
    match node {
        mdast::Node::Root(root) => root
            .children
            .iter()
            .map(mdast_to_markdown)
            .collect::<Vec<_>>()
            .join("\n\n"),
        mdast::Node::Paragraph(para) => para
            .children
            .iter()
            .map(mdast_to_markdown)
            .collect::<Vec<_>>()
            .join(""),
        mdast::Node::Text(text) => text.value.clone(),
        mdast::Node::Strong(strong) => {
            format!(
                "**{}**",
                strong
                    .children
                    .iter()
                    .map(mdast_to_markdown)
                    .collect::<Vec<_>>()
                    .join("")
            )
        }
        mdast::Node::Emphasis(emphasis) => {
            format!(
                "*{}*",
                emphasis
                    .children
                    .iter()
                    .map(mdast_to_markdown)
                    .collect::<Vec<_>>()
                    .join("")
            )
        }
        mdast::Node::Code(code) => {
            format!("`{}`", code.value)
        }
        mdast::Node::Link(link) => {
            format!(
                "[{}]({})",
                link.children
                    .iter()
                    .map(mdast_to_markdown)
                    .collect::<Vec<_>>()
                    .join(""),
                link.url
            )
        }
        _ => String::new(), // Handle other node types as needed
    }
}

/// `value` as text in HTML: the five characters that could start or end
/// markup or an attribute, as entities.
fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#x27;"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod safety_tests {
    use super::{escape_html, process_markdown_content};

    /// What someone writes cannot become markup: raw HTML is shown as
    /// text and a javascript: link loses its target.
    #[test]
    fn markdown_written_by_anyone_stays_text() {
        let html = process_markdown_content(
            "<script>steal()</script> <img src=x onerror=steal()> [go](javascript:steal())",
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("<img"), "{html}");
        assert!(!html.contains("javascript:"), "{html}");
    }

    #[test]
    fn text_that_is_escaped_cannot_open_a_tag_or_close_an_attribute() {
        assert_eq!(
            escape_html(r#"<a href="x" title='y'>&"#),
            "&lt;a href=&quot;x&quot; title=&#x27;y&#x27;&gt;&amp;"
        );
    }
}
