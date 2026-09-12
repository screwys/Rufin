use std::collections::BTreeSet;

// Web calls use JSON string literals, shared by JavaScript and Askama.
pub fn messages(source: &str) -> BTreeSet<String> {
    ["tr(", "msgid("]
        .into_iter()
        .flat_map(|marker| {
            source
                .match_indices(marker)
                .map(move |(index, _)| index + marker.len())
        })
        .filter_map(|index| {
            let source = source.get(index..)?.trim_start();
            if let Some(single) = source.strip_prefix('\'') {
                let mut quoted = String::from("\"");
                let mut chars = single.chars();
                while let Some(character) = chars.next() {
                    match character {
                        '\'' => {
                            quoted.push('"');
                            return serde_json::from_str(&quoted).ok();
                        }
                        '"' => quoted.push_str("\\\""),
                        '\\' => {
                            let next = chars.next()?;
                            if next != '\'' {
                                quoted.push('\\');
                            }
                            quoted.push(next);
                        }
                        _ => quoted.push(character),
                    }
                }
                None
            } else {
                serde_json::Deserializer::from_str(source)
                    .into_iter::<String>()
                    .next()
                    .and_then(Result::ok)
            }
        })
        .collect()
}
