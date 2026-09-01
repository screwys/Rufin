use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use proc_macro2::{TokenStream, TokenTree};
use roxmltree::Document;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, ExprLit, Lit, Token};

use crate::Result;
use crate::process::{collect_files_with_extension, read_to_string};

const TEMPLATE_HEADER: &str = "# Rufin translation template.\n\
# Copyright (C) 2026 Rufin contributors\n\
# This file is distributed under the same license as the Rufin package.\n\
#\n\
#, fuzzy\n\
msgid \"\"\n\
msgstr \"\"\n\
\"Project-Id-Version: Rufin\\n\"\n\
\"Report-Msgid-Bugs-To: https://github.com/screwys/Rufin/issues\\n\"\n\
\"POT-Creation-Date: YEAR-MO-DA HO:MI+ZONE\\n\"\n\
\"PO-Revision-Date: YEAR-MO-DA HO:MI+ZONE\\n\"\n\
\"Last-Translator: Rufin translators\\n\"\n\
\"Language-Team: Rufin translators\\n\"\n\
\"Language: \\n\"\n\
\"MIME-Version: 1.0\\n\"\n\
\"Content-Type: text/plain; charset=UTF-8\\n\"\n\
\"Content-Transfer-Encoding: 8bit\\n\"\n";

const SINGULAR_KEYWORDS: &[(&str, usize)] = &[
    ("tr", 0),
    ("tr_with", 0),
    ("msgid", 0),
    ("text_button", 1),
    ("icon_button", 1),
    ("icon_button_without_tooltip", 1),
    ("detail_action_button", 1),
    ("detail_link_button", 1),
    ("toggle_button", 1),
    ("row_button", 1),
    ("cover_hover_controls", 1),
    ("table_header_label", 0),
    ("button_row", 0),
    ("dialog_button", 0),
    ("labeled_control", 0),
    ("labeled_row", 0),
    ("smart_playlist_dialog", 0),
];

const PLURAL_KEYWORDS: &[&str] = &["trn", "trn_with"];

#[derive(Clone, Debug, Default)]
struct Message {
    plural: Option<String>,
    comments: BTreeSet<String>,
    rust_format: bool,
}

#[derive(Default)]
struct Catalog {
    messages: BTreeMap<(String, String), Message>,
}

impl Catalog {
    fn insert(
        &mut self,
        context: Option<&str>,
        message: String,
        plural: Option<String>,
        comment: Option<&str>,
        rust_format: bool,
    ) -> Result<()> {
        if message.is_empty() {
            return Ok(());
        }
        let key = (context.unwrap_or_default().to_owned(), message.clone());
        let entry = self.messages.entry(key).or_default();
        if entry.plural.is_some() && plural.is_some() && entry.plural != plural {
            return Err(format!("conflicting plural forms for gettext message: {message}").into());
        }
        if entry.plural.is_none() {
            entry.plural = plural;
        }
        if let Some(comment) = comment.filter(|comment| !comment.is_empty()) {
            entry.comments.insert(comment.to_owned());
        }
        entry.rust_format |= rust_format;
        Ok(())
    }
}

pub(crate) fn template(root: &Path) -> Result<String> {
    let mut catalog = Catalog::default();
    extract_rust(root, &mut catalog)?;
    extract_builder(root, &mut catalog)?;
    render(&catalog)
}

fn extract_rust(root: &Path, catalog: &mut Catalog) -> Result<()> {
    let mut files = Vec::new();
    collect_files_with_extension(root, &root.join("crates"), "rs", &mut files)?;
    files.sort();
    for file in files {
        let source = read_to_string(&file)?;
        let syntax = syn::parse_file(&source)
            .map_err(|error| format!("could not parse {} for gettext: {error}", file.display()))?;
        let mut extractor = RustExtractor {
            catalog,
            errors: Vec::new(),
        };
        extractor.visit_file(&syntax);
        if !extractor.errors.is_empty() {
            return Err(extractor.errors.join("\n").into());
        }
    }
    Ok(())
}

struct RustExtractor<'a> {
    catalog: &'a mut Catalog,
    errors: Vec<String>,
}

impl Visit<'_> for RustExtractor<'_> {
    fn visit_expr_call(&mut self, call: &ExprCall) {
        let name = match call.func.as_ref() {
            Expr::Path(path) => path
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string()),
            _ => None,
        };
        if let Some(name) = name {
            extract_call(self.catalog, &mut self.errors, &name, call.args.iter());
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_macro(&mut self, node: &syn::Macro) {
        extract_token_calls(self.catalog, &mut self.errors, node.tokens.clone());
        visit::visit_macro(self, node);
    }
}

fn extract_call<'a>(
    catalog: &mut Catalog,
    errors: &mut Vec<String>,
    name: &str,
    arguments: impl Iterator<Item = &'a Expr>,
) {
    let arguments = arguments.collect::<Vec<_>>();
    let message = if let Some((_, argument)) = SINGULAR_KEYWORDS
        .iter()
        .find(|(keyword, _)| *keyword == name)
    {
        arguments
            .get(*argument)
            .and_then(|argument| literal_string(argument))
            .map(|message| (message, None))
    } else if PLURAL_KEYWORDS.contains(&name) {
        arguments
            .first()
            .and_then(|argument| literal_string(argument))
            .zip(
                arguments
                    .get(1)
                    .and_then(|argument| literal_string(argument)),
            )
            .map(|(message, plural)| (message, Some(plural)))
    } else {
        None
    };
    if let Some((message, plural)) = message
        && let Err(error) =
            catalog.insert(None, message.clone(), plural, None, rust_format(&message))
    {
        errors.push(error.to_string());
    }
}

fn extract_token_calls(catalog: &mut Catalog, errors: &mut Vec<String>, tokens: TokenStream) {
    let tokens = tokens.into_iter().collect::<Vec<_>>();
    for (index, token) in tokens.iter().enumerate() {
        if let TokenTree::Ident(identifier) = token
            && let Some(TokenTree::Group(arguments)) = tokens.get(index + 1)
            && let Ok(arguments) =
                Punctuated::<Expr, Token![,]>::parse_terminated.parse2(arguments.stream())
        {
            extract_call(catalog, errors, &identifier.to_string(), arguments.iter());
        }
        if let TokenTree::Group(group) = token {
            extract_token_calls(catalog, errors, group.stream());
        }
    }
}

fn literal_string(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Lit(ExprLit {
            lit: Lit::Str(value),
            ..
        }) => Some(value.value()),
        Expr::Group(group) => literal_string(&group.expr),
        Expr::Paren(paren) => literal_string(&paren.expr),
        _ => None,
    }
}

fn extract_builder(root: &Path, catalog: &mut Catalog) -> Result<()> {
    let resource_root = root.join("crates/ui/resources");
    let mut files = Vec::new();
    collect_files_with_extension(root, &resource_root, "ui", &mut files)?;
    files.sort();
    for file in files {
        let source = read_to_string(&file)?;
        let document = Document::parse(&source).map_err(|error| {
            format!(
                "could not parse {} for GtkBuilder gettext: {error}",
                file.display()
            )
        })?;
        extract_builder_document(&document, catalog)?;
    }
    Ok(())
}

fn extract_builder_document(document: &Document<'_>, catalog: &mut Catalog) -> Result<()> {
    for node in document.descendants().filter(|node| node.is_element()) {
        if !node
            .attribute("translatable")
            .is_some_and(translatable_attribute)
        {
            continue;
        }
        let message = node
            .children()
            .filter_map(|child| child.text())
            .collect::<String>();
        catalog.insert(
            node.attribute("context"),
            message,
            None,
            node.attribute("comments"),
            false,
        )?;
    }
    Ok(())
}

fn translatable_attribute(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "t" | "yes" | "y" | "1"
    )
}

fn rust_format(message: &str) -> bool {
    let bytes = message.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'{' {
            if bytes.get(index + 1) == Some(&b'{') {
                index += 2;
                continue;
            }
            return bytes[index + 1..].contains(&b'}');
        }
        index += 1;
    }
    false
}

fn render(catalog: &Catalog) -> Result<String> {
    let mut output = String::from(TEMPLATE_HEADER);
    let mut messages = catalog.messages.iter().collect::<Vec<_>>();
    messages.sort_by(
        |((left_context, left_message), _), ((right_context, right_message), _)| {
            field_sort_key("msgctxt", left_context)
                .cmp(&field_sort_key("msgctxt", right_context))
                .then_with(|| {
                    field_sort_key("msgid", left_message)
                        .cmp(&field_sort_key("msgid", right_message))
                })
        },
    );
    for ((context, message), entry) in messages {
        output.push('\n');
        for comment in &entry.comments {
            for line in comment.lines() {
                writeln!(output, "#. {line}")?;
            }
        }
        if entry.rust_format {
            output.push_str("#, rust-format\n");
        }
        if !context.is_empty() {
            write_field(&mut output, "msgctxt", context)?;
        }
        write_field(&mut output, "msgid", message)?;
        if let Some(plural) = &entry.plural {
            write_field(&mut output, "msgid_plural", plural)?;
            output.push_str("msgstr[0] \"\"\nmsgstr[1] \"\"\n");
        } else {
            output.push_str("msgstr \"\"\n");
        }
    }
    Ok(output)
}

fn field_sort_key(name: &str, value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let mut field = String::new();
    write_field(&mut field, name, value).expect("writing to a string cannot fail");
    let prefix = format!("{name} ");
    let mut key = String::new();
    for (index, line) in field.lines().enumerate() {
        if index == 0 {
            key.push_str(line.strip_prefix(&prefix).unwrap_or(line));
        } else if line.starts_with('"') {
            key.push_str(line);
        }
    }
    key
}

fn write_field(output: &mut String, name: &str, value: &str) -> Result<()> {
    let escaped = escape(value);
    if value.matches('\n').count() <= 1 && name.len() + escaped.chars().count() <= 78 {
        writeln!(output, "{name} \"{escaped}\"")?;
        return Ok(());
    }
    writeln!(output, "{name} \"\"")?;
    for line in escaped.split_inclusive("\\n") {
        for folded in wrap(line) {
            writeln!(output, "\"{folded}\"")?;
        }
    }
    Ok(())
}

fn escape(value: &str) -> String {
    let mut escaped = String::new();
    for byte in value.bytes() {
        match byte {
            b'\\' => escaped.push_str("\\\\"),
            b'\"' => escaped.push_str("\\\""),
            b'\n' => escaped.push_str("\\n"),
            b'\r' => escaped.push_str("\\r"),
            b'\t' => escaped.push_str("\\t"),
            0x20..=0x7e => escaped.push(char::from(byte)),
            _ => write!(escaped, "\\{byte:03o}").expect("writing to a string cannot fail"),
        }
    }
    escaped
}

fn wrap(value: &str) -> Vec<&str> {
    let mut spaces = value
        .match_indices(' ')
        .map(|(index, _)| index + 1)
        .collect::<Vec<_>>();
    spaces.insert(0, 0);
    if spaces.last().copied().unwrap_or_default() < value.len() {
        spaces.push(value.len());
    }
    let mut result = Vec::new();
    let mut previous_width = 0;
    let mut previous_index = 0;
    let mut line_start = 0;
    let mut spaces = spaces.iter().peekable();
    while let Some(begin) = spaces.next() {
        let Some(end) = spaces.peek() else {
            break;
        };
        let segment_width = value[*begin..**end].chars().count();
        if previous_index == 0 || previous_width + segment_width <= 77 {
            previous_width += segment_width;
            previous_index = **end;
        } else {
            result.push(&value[line_start..previous_index]);
            line_start = previous_index;
            previous_index = **end;
            previous_width = segment_width;
        }
    }
    result.push(&value[line_start..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gettext_output_escapes_and_wraps_like_the_template() {
        let mut output = String::new();
        write_field(&mut output, "msgid", r"No results ¯\_(°╭╮°)_/¯").unwrap();
        assert_eq!(
            output,
            "msgid \"\"\n\
             \"No results \\302\\257\\\\_(\\302\\260\\342\\225\\255\\342\\225\\256\\302\\260)_/\\302\\257\"\n"
        );
    }

    #[test]
    fn rust_and_builder_extractors_share_one_catalog() {
        let syntax = syn::parse_file(
            r#"fn example() {
                tr("Play");
                trn("{count} track", "{count} tracks", 2);
                format!("{}", tr("Added to"));
            }"#,
        )
        .unwrap();
        let mut catalog = Catalog::default();
        let mut extractor = RustExtractor {
            catalog: &mut catalog,
            errors: Vec::new(),
        };
        extractor.visit_file(&syntax);
        assert!(extractor.errors.is_empty());
        let builder = Document::parse(
            r#"<interface><property translatable="yes" context="button">Search</property></interface>"#,
        )
        .unwrap();
        extract_builder_document(&builder, &mut catalog).unwrap();
        assert!(
            catalog
                .messages
                .contains_key(&(String::new(), "Play".into()))
        );
        assert!(
            catalog
                .messages
                .contains_key(&(String::new(), "Added to".into()))
        );
        assert!(
            catalog
                .messages
                .contains_key(&("button".into(), "Search".into()))
        );
        assert!(
            catalog
                .messages
                .get(&(String::new(), "{count} track".into()))
                .is_some_and(|entry| entry.plural.as_deref() == Some("{count} tracks"))
        );
    }
}
