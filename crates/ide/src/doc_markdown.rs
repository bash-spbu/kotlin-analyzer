//! Doc comments as the markdown an editor's hover renders (CommonMark:
//! paragraphs, emphasis, code spans and blocks, links, lists, headings).
//! KDoc is markdown already: its `[name]` references become code. Javadoc
//! is HTML with tags of its own: `<p>`, `<pre>`, `<ul>`, `<a href>`,
//! `{@code …}`, `{@link …}` are mapped, other HTML dropped, entities
//! decoded. Block tags (`@param`, `@return`, `@throws`, `@see`, …) of both
//! become sections after the description.

use syntax::Language;

/// `comment` (`/** … */`, frame and all) as markdown.
pub(crate) fn to_markdown(comment: &str, language: Language) -> String {
    let lines = comment_lines(comment);
    let (description, tags) = split_block_tags(&lines, language);
    let mut sections = Vec::new();
    let description = text_to_markdown(&description.join("\n"), language);
    if !description.is_empty() {
        sections.push(description);
    }
    sections.extend(tag_sections(&tags, language));
    sections.join("\n\n")
}

/// The comment's lines without `/**`, `*/` and each line's leading `* `;
/// the indentation after that stays (it matters in code blocks).
fn comment_lines(comment: &str) -> Vec<String> {
    let body = comment.trim_start_matches("/**").trim_end_matches("*/");
    let lines: Vec<String> = body
        .lines()
        .map(|line| {
            let line = line.trim_start();
            let after_star = line.strip_prefix('*').unwrap_or(line);
            let after_space = after_star.strip_prefix(' ').unwrap_or(after_star);
            after_space.trim_end().to_owned()
        })
        .collect();
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    match (first, last) {
        (Some(first), Some(last)) => lines[first..=last].to_vec(),
        _ => Vec::new(),
    }
}

/// A block tag: `@param amount the amount`, continued on the lines up to
/// the next one.
#[derive(Debug, PartialEq, Eq)]
struct BlockTag {
    name: String,
    text: String,
}

/// The description, then the block tags — a line starting with `@word`
/// outside a code block starts one.
fn split_block_tags(lines: &[String], language: Language) -> (Vec<String>, Vec<BlockTag>) {
    let mut description = Vec::new();
    let mut tags: Vec<BlockTag> = Vec::new();
    let mut in_code = false;
    for line in lines {
        let trimmed = line.trim_start();
        let tag_name = trimmed
            .strip_prefix('@')
            .map(|rest| {
                rest.split(|char: char| !char.is_alphanumeric())
                    .next()
                    .unwrap_or("")
            })
            .filter(|name| !name.is_empty());
        match tag_name {
            Some(name) if !in_code => tags.push(BlockTag {
                name: name.to_owned(),
                text: trimmed[1 + name.len()..].trim().to_owned(),
            }),
            _ => match tags.last_mut() {
                Some(tag) => {
                    tag.text.push('\n');
                    tag.text.push_str(line);
                }
                None => description.push(line.clone()),
            },
        }
        in_code = code_after(line, in_code, language);
    }
    (description, tags)
}

/// Whether a code block is open after `line`: a ```` ``` ```` fence in
/// KDoc, `<pre>` … `</pre>` in Javadoc.
fn code_after(line: &str, in_code: bool, language: Language) -> bool {
    match language {
        Language::Kotlin => in_code != line.trim_start().starts_with("```"),
        Language::Java => {
            let opened = line.rfind("<pre");
            let closed = line.rfind("</pre>");
            match (opened, closed) {
                (Some(opened), Some(closed)) => opened > closed,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => in_code,
            }
        }
    }
}

fn text_to_markdown(text: &str, language: Language) -> String {
    match language {
        Language::Kotlin => kdoc_references(text).trim().to_owned(),
        Language::Java => tidy(&javadoc_html(text)),
    }
}

/// KDoc's `[name]` and `[Class.member]` as code; a markdown link
/// (`[text](url)`, `[text][label]`) stays.
fn kdoc_references(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    let mut in_fence = false;
    while !rest.is_empty() {
        let line_end = rest.find('\n').map_or(rest.len(), |index| index + 1);
        let (line, after) = rest.split_at(line_end);
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if in_fence || line.trim_start().starts_with("```") {
            output.push_str(line);
        } else {
            output.push_str(&line_references(line));
        }
        rest = after;
    }
    output
}

fn line_references(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        output.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find(']') else {
            output.push_str(&rest[open..]);
            return output;
        };
        let reference = &after_open[..close];
        let following = &after_open[close + 1..];
        let is_link = following.starts_with('(') || following.starts_with('[');
        let is_name = !reference.is_empty()
            && reference
                .chars()
                .all(|char| char.is_alphanumeric() || matches!(char, '.' | '_' | '`' | '#'));
        if is_name && !is_link {
            output.push_str(&code_span(&reference.replace('#', ".")));
        } else {
            output.push('[');
            output.push_str(reference);
            output.push(']');
        }
        rest = following;
    }
    output.push_str(rest);
    output
}

/// Javadoc's HTML and inline tags as markdown.
fn javadoc_html(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut lists: Vec<OpenList> = Vec::new();
    let mut link_targets: Vec<Option<String>> = Vec::new();
    let mut rest = text;
    while let Some(char) = rest.chars().next() {
        if rest.starts_with("{@")
            && let Some((tag, after)) = inline_tag(rest)
        {
            output.push_str(&inline_tag_markdown(&tag));
            rest = after;
            continue;
        }
        if char == '<'
            && let Some((tag, after)) = html_tag(rest)
        {
            if tag.name == "pre" && !tag.closing {
                let (code, after_code) = pre_block(after);
                output.push_str(&format!("\n\n```java\n{code}\n```\n\n"));
                rest = after_code;
                continue;
            }
            // List items follow one another without blank lines between.
            let follows_an_item = lists.last().is_some_and(|list| list.items > 0);
            if tag.name == "li" && !tag.closing && follows_an_item {
                output.truncate(output.trim_end().len());
            }
            output.push_str(&html_tag_markdown(&tag, &mut lists, &mut link_targets));
            rest = after;
            continue;
        }
        if char == '&'
            && let Some((decoded, after)) = entity(rest)
        {
            output.push(decoded);
            rest = after;
            continue;
        }
        output.push(char);
        rest = &rest[char.len_utf8()..];
    }
    output
}

/// An `<ul>` or `<ol>` being mapped, and how many items it has had.
#[derive(Clone, Copy)]
struct OpenList {
    numbered: bool,
    items: u32,
}

struct HtmlTag {
    name: String,
    closing: bool,
    /// `href="…"` of an `<a>`.
    href: Option<String>,
}

/// The HTML tag `text` starts with (`<p>`, `</b>`, `<a href="…">`, `<br/>`)
/// and what follows it; `None` for a `<` that starts none (`a < b`).
fn html_tag(text: &str) -> Option<(HtmlTag, &str)> {
    let end = text.find('>')?;
    let inside = &text[1..end];
    let closing = inside.starts_with('/');
    let inside = inside.trim_start_matches('/').trim_end_matches('/');
    let name: String = inside
        .chars()
        .take_while(|char| char.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    let starts_with_letter = name.starts_with(|char: char| char.is_ascii_alphabetic());
    let attributes = &inside[name.len()..];
    let is_tag = starts_with_letter
        && (attributes.is_empty() || attributes.starts_with(char::is_whitespace));
    if !is_tag {
        return None;
    }
    let href = attributes
        .split("href=")
        .nth(1)
        .map(|value| value.trim_start_matches(['"', '\'']))
        .and_then(|value| value.split(['"', '\'']).next())
        .map(str::to_owned);
    Some((
        HtmlTag {
            name,
            closing,
            href,
        },
        &text[end + 1..],
    ))
}

fn html_tag_markdown(
    tag: &HtmlTag,
    lists: &mut Vec<OpenList>,
    link_targets: &mut Vec<Option<String>>,
) -> String {
    match (tag.name.as_str(), tag.closing) {
        ("p", _) | ("div", _) | ("blockquote", _) | ("table", _) | ("dl", _) => "\n\n".to_owned(),
        ("br", _) => "  \n".to_owned(),
        ("b" | "strong", _) => "**".to_owned(),
        ("i" | "em" | "cite" | "var", _) => "*".to_owned(),
        ("code" | "tt" | "samp" | "kbd", _) => "`".to_owned(),
        ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => "\n\n**".to_owned(),
        ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => "**\n\n".to_owned(),
        ("ul" | "ol", false) => {
            lists.push(OpenList {
                numbered: tag.name == "ol",
                items: 0,
            });
            "\n\n".to_owned()
        }
        ("ul" | "ol", true) => {
            lists.pop();
            "\n\n".to_owned()
        }
        ("li", false) => {
            let indent = "  ".repeat(lists.len().saturating_sub(1));
            match lists.last_mut() {
                Some(list) => {
                    list.items += 1;
                    if list.numbered {
                        format!("\n{indent}{}. ", list.items)
                    } else {
                        format!("\n{indent}- ")
                    }
                }
                None => "\n- ".to_owned(),
            }
        }
        ("dt", false) => "\n\n**".to_owned(),
        ("dt", true) => "**".to_owned(),
        ("dd", false) => "\n".to_owned(),
        ("tr", false) => "\n".to_owned(),
        ("td" | "th", false) => " ".to_owned(),
        ("a", false) => {
            link_targets.push(tag.href.clone());
            if tag.href.is_some() { "[" } else { "" }.to_owned()
        }
        ("a", true) => match link_targets.pop().flatten() {
            Some(href) => format!("]({href})"),
            None => String::new(),
        },
        _ => String::new(),
    }
}

/// A `<pre>` block's code, up to its `</pre>`: entities decoded, a
/// `{@code …}` around it unwrapped, other tags dropped, its common
/// indentation removed.
fn pre_block(text: &str) -> (String, &str) {
    let (inside, after) = match text.find("</pre>") {
        Some(end) => (&text[..end], &text[end + "</pre>".len()..]),
        None => (text, ""),
    };
    let mut inside = inside.trim_matches('\n');
    let wrapped = inside.trim_start().starts_with("{@code") && inside.trim_end().ends_with('}');
    if wrapped {
        let start = inside.find("{@code").unwrap_or(0) + "{@code".len();
        let end = inside.rfind('}').unwrap_or(inside.len());
        inside = inside[start..end].trim_matches('\n');
    }
    let mut code = String::with_capacity(inside.len());
    let mut rest = inside;
    while let Some(char) = rest.chars().next() {
        if char == '<'
            && !wrapped
            && let Some((_, after_tag)) = html_tag(rest)
        {
            rest = after_tag;
            continue;
        }
        if char == '&'
            && let Some((decoded, after_entity)) = entity(rest)
        {
            code.push(decoded);
            rest = after_entity;
            continue;
        }
        code.push(char);
        rest = &rest[char.len_utf8()..];
    }
    (dedent(&code), after)
}

fn dedent(code: &str) -> String {
    let indent = code
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    code.lines()
        .map(|line| line.get(indent..).unwrap_or(line.trim_start()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `&lt;`, `&#64;`, `&#x40;` and the like, and what follows it.
fn entity(text: &str) -> Option<(char, &str)> {
    let end = text.find(';').filter(|end| *end <= 10)?;
    let name = &text[1..end];
    let decoded = match name {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "copy" => '©',
        _ => {
            let number = name.strip_prefix('#')?;
            let value = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(value)?
        }
    };
    Some((decoded, &text[end + 1..]))
}

/// An inline tag `{@kind argument}` (braces in it balanced) and what
/// follows it.
fn inline_tag(text: &str) -> Option<((String, String), &str)> {
    let mut depth = 0usize;
    for (index, char) in text.char_indices() {
        match char {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let inside = &text[2..index];
                    let (kind, argument) = inside
                        .split_once(char::is_whitespace)
                        .unwrap_or((inside, ""));
                    return Some((
                        (kind.to_owned(), argument.trim().to_owned()),
                        &text[index + 1..],
                    ));
                }
            }
            _ => {}
        }
    }
    None
}

fn inline_tag_markdown((kind, argument): &(String, String)) -> String {
    match kind.as_str() {
        "code" => code_span(&decode_entities(argument)),
        "literal" => decode_entities(argument),
        "link" | "linkplain" => {
            let (target, label) = argument
                .split_once(char::is_whitespace)
                .map_or((argument.as_str(), ""), |(target, label)| {
                    (target, label.trim())
                });
            let target = target.trim_start_matches('#').replace('#', ".");
            match (kind.as_str(), label.is_empty()) {
                (_, true) => code_span(&target),
                ("link", false) => code_span(label),
                (_, false) => label.to_owned(),
            }
        }
        "value" => code_span(&argument.replace('#', ".")),
        "inheritDoc" | "docRoot" => String::new(),
        _ => argument.clone(),
    }
}

fn decode_entities(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(char) = rest.chars().next() {
        if char == '&'
            && let Some((decoded, after)) = entity(rest)
        {
            output.push(decoded);
            rest = after;
            continue;
        }
        output.push(char);
        rest = &rest[char.len_utf8()..];
    }
    output
}

/// `text` as a code span, with enough backticks around it for the ones in it.
fn code_span(text: &str) -> String {
    let fence = if text.contains('`') { "``" } else { "`" };
    let padding = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{padding}{text}{padding}{fence}")
}

/// Mapped Javadoc made tidy: outside code blocks each line loses its
/// indentation (four spaces would make a code block) and runs of blank
/// lines become one.
fn tidy(markdown: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        let is_fence = line.trim_start().starts_with("```");
        if in_fence || is_fence {
            lines.push(line.to_owned());
        } else {
            let is_list_item = line.trim_start().starts_with("- ")
                || line
                    .trim_start()
                    .split_once(". ")
                    .is_some_and(|(number, _)| {
                        !number.is_empty() && number.chars().all(|char| char.is_ascii_digit())
                    });
            // A nested list item keeps its two-space steps.
            let kept_indent = if is_list_item {
                (line.len() - line.trim_start().len()) / 2 * 2
            } else {
                0
            };
            lines.push(format!("{}{}", " ".repeat(kept_indent.min(6)), line.trim()));
        }
        if is_fence {
            in_fence = !in_fence;
        }
    }
    let mut output = String::new();
    let mut blank_before = false;
    for line in lines {
        let blank = line.trim().is_empty();
        if blank && (blank_before || output.is_empty()) {
            continue;
        }
        output.push_str(&line);
        output.push('\n');
        blank_before = blank;
    }
    output.trim().to_owned()
}

/// The block tags as sections: deprecation, parameters, what it returns,
/// what it throws, references, since when, by whom.
fn tag_sections(tags: &[BlockTag], language: Language) -> Vec<String> {
    let text = |tag: &BlockTag| text_to_markdown(&tag.text, language);
    let named = |names: &[&str]| -> Vec<&BlockTag> {
        tags.iter()
            .filter(|tag| names.contains(&tag.name.as_str()))
            .collect()
    };
    let mut sections = Vec::new();
    for tag in named(&["deprecated"]) {
        sections.push(format!("**Deprecated.** {}", text(tag)).trim().to_owned());
    }
    let listed = |title: &str, names: &[&str], sections: &mut Vec<String>| {
        let items: Vec<String> = named(names)
            .iter()
            .map(|tag| {
                let (subject, description) = tag_subject(&tag.text);
                let description = text_to_markdown(description, language);
                let subject = code_span(&subject.replace('#', "."));
                if description.is_empty() {
                    format!("- {subject}")
                } else {
                    format!("- {subject} — {}", description.replace('\n', " "))
                }
            })
            .collect();
        if !items.is_empty() {
            sections.push(format!("**{title}**\n{}", items.join("\n")));
        }
    };
    listed("Parameters", &["param"], &mut sections);
    listed("Properties", &["property"], &mut sections);
    for tag in named(&["return", "returns"]) {
        sections.push(format!("**Returns** {}", text(tag)));
    }
    for tag in named(&["receiver"]) {
        sections.push(format!("**Receiver** {}", text(tag)));
    }
    for tag in named(&["constructor"]) {
        sections.push(format!("**Constructor** {}", text(tag)));
    }
    listed("Throws", &["throws", "exception"], &mut sections);
    let references: Vec<String> = named(&["see", "sample"])
        .iter()
        .map(|tag| {
            let reference = tag.text.trim();
            let is_plain_text = reference.starts_with('"') || reference.starts_with('<');
            if is_plain_text {
                format!(
                    "- {}",
                    text_to_markdown(reference, language).trim_matches('"')
                )
            } else {
                let (target, label) = tag_subject(reference);
                let target = code_span(&target.trim_start_matches('#').replace('#', "."));
                match label.trim() {
                    "" => format!("- {target}"),
                    label => format!("- {target} — {label}"),
                }
            }
        })
        .collect();
    if !references.is_empty() {
        sections.push(format!("**See also**\n{}", references.join("\n")));
    }
    for tag in named(&["since"]) {
        sections.push(format!("**Since** {}", text(tag)));
    }
    let authors: Vec<String> = named(&["author"]).iter().map(|tag| text(tag)).collect();
    if !authors.is_empty() {
        sections.push(format!("**Author** {}", authors.join(", ")));
    }
    sections
}

/// A tag's subject and the rest: `amount the amount` (Java), `[amount] the
/// amount` (KDoc).
fn tag_subject(text: &str) -> (String, &str) {
    let text = text.trim_start();
    if let Some(after_open) = text.strip_prefix('[')
        && let Some(close) = after_open.find(']')
    {
        return (
            after_open[..close].to_owned(),
            after_open[close + 1..].trim_start(),
        );
    }
    match text.split_once(char::is_whitespace) {
        Some((subject, rest)) => (subject.to_owned(), rest.trim_start()),
        None => (text.to_owned(), ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn javadoc_html_and_tags_become_markdown() {
        let comment = r#"/**
     * Indicates that an annotated class is a "component".
     *
     * <p>As of Spring Framework 6.1, support for configuring the name of a
     * stereotype by convention (i.e., via a {@code String value()} attribute
     * without {@code @AliasFor}) is deprecated. See {@link Repository#value()}
     * and {@link org.example.Advice#name() the advice's name}.
     * <ul>
     *   <li><b>first</b> &amp; <i>second</i></li>
     *   <li>see <a href="https://spring.io/docs">the docs</a></li>
     * </ul>
     * <pre class="code">
     * &#064;Component
     * public class Example {
     *     List&lt;String&gt; names;
     * }
     * </pre>
     *
     * @param <T> the type
     * @param name the bean's
     *     name, if any
     * @return the {@code Component}
     * @throws IllegalStateException when it can't
     * @author Mark Fisher
     * @author Sam Brannen
     * @since 2.5
     * @see Repository
     * @see org.springframework.context.annotation.ClassPathBeanDefinitionScanner
     */"#;
        let expected = "Indicates that an annotated class is a \"component\".

As of Spring Framework 6.1, support for configuring the name of a
stereotype by convention (i.e., via a `String value()` attribute
without `@AliasFor`) is deprecated. See `Repository.value()`
and `the advice's name`.

- **first** & *second*
- see [the docs](https://spring.io/docs)

```java
@Component
public class Example {
    List<String> names;
}
```

**Parameters**
- `<T>` — the type
- `name` — the bean's name, if any

**Returns** the `Component`

**Throws**
- `IllegalStateException` — when it can't

**See also**
- `Repository`
- `org.springframework.context.annotation.ClassPathBeanDefinitionScanner`

**Since** 2.5

**Author** Mark Fisher, Sam Brannen";
        assert_eq!(to_markdown(comment, Language::Java), expected);
    }

    #[test]
    fn a_pre_with_code_inside_keeps_its_generics_and_a_less_than_isnt_a_tag() {
        let comment = "/**\n * Use it when a < b:\n * <pre>{@code\n * Map<String, List<Integer>> map = new HashMap<>();\n * }</pre>\n */";
        assert_eq!(
            to_markdown(comment, Language::Java),
            "Use it when a < b:\n\n```java\nMap<String, List<Integer>> map = new HashMap<>();\n```"
        );
    }

    #[test]
    fn kdoc_stays_markdown_with_references_as_code_and_tags_as_sections() {
        let comment = "/**\n * Charges [amount] to [Account.owner], see [the guide](https://example.com).\n *\n * ```kotlin\n * charge(amount = [1])\n * ```\n * @param amount in minor units\n * @param[currency] ISO code\n * @return the [Receipt]\n */";
        assert_eq!(
            to_markdown(comment, Language::Kotlin),
            "Charges `amount` to `Account.owner`, see [the guide](https://example.com).\n\n```kotlin\ncharge(amount = [1])\n```\n\n**Parameters**\n- `amount` — in minor units\n- `currency` — ISO code\n\n**Returns** the `Receipt`"
        );
    }

    #[test]
    fn a_comment_of_one_line_or_none() {
        assert_eq!(
            to_markdown("/** Returns the total. */", Language::Kotlin),
            "Returns the total."
        );
        assert_eq!(to_markdown("/** */", Language::Java), "");
    }
}
