//! How Rufin tells that two names are the same: how a character folds, one key to compare by,
//! the artists a credit names, and a title without what marks a version of it.

use unicode_normalization::UnicodeNormalization;

/// A name's words as they compare: letters and digits, whatever their case, width or Latin
/// accents, one space between what anything else parted.
pub fn name_words(name: &str) -> String {
    let mut folded = String::with_capacity(name.len());
    for character in name.nfkc() {
        if character.is_alphanumeric() {
            fold(character, &mut folded);
        } else {
            folded.push(' ');
        }
    }
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A name as it compares: its letters and digits, whatever their case, width, Latin accents or
/// punctuation; a name of punctuation only, such as "&", by its characters.
pub fn name_key(name: &str) -> String {
    let key = name_words(name).replace(' ', "");
    if key.is_empty() {
        name.nfkc()
            .flat_map(char::to_lowercase)
            .filter(|character| !character.is_whitespace())
            .collect()
    } else {
        key
    }
}

/// A combining mark that `unicode61` folds away inside a word.
pub fn diacritic(character: char) -> bool {
    let code = character as u32;
    match code {
        768..=799 => 0x0802_9FDF_u32 >> (code - 768) & 1 == 1,
        800..=817 => 0x0003_61F8_u32 >> (code - 800) & 1 == 1,
        _ => false,
    }
}

/// Folds a word character as `unicode61 remove_diacritics=2` does: lower case, and a Latin
/// letter with diacritics becomes its base letter.
pub fn fold(character: char, folded: &mut String) {
    for lower in character.to_lowercase() {
        let mut base = None;
        let mut marks = true;
        unicode_normalization::char::decompose_canonical(lower, |part| match base {
            None => base = Some(part),
            Some(_) => marks &= unicode_normalization::char::is_combining_mark(part),
        });
        match base {
            _ if diacritic(lower) => {}
            Some(base) if u32::from(lower) < 0x2000 && base.is_ascii_alphabetic() && marks => {
                folded.push(base);
            }
            _ => folded.push(lower),
        }
    }
}

/// What parts one credit's artists, lowercase; each keeps its byte length in any case.
const ARTIST_SEPARATORS: &[&str] = &[
    " feat. ",
    " feat ",
    " featuring ",
    " ft. ",
    " ft ",
    " with ",
    " vs. ",
    " vs ",
    " x ",
    " × ",
    " & ",
    " ＆ ",
    " / ",
    " | ",
    ", ",
    "、",
    ";",
    "•",
    "·",
];

/// The artists a credit names, in its order: "A feat. B", "A, B" and "A & B" each name two, and
/// a character with its voice, "X (CV. Y)", names both. A band whose name holds a separator
/// parts too, so the whole credit stays the first thing to try.
pub fn credit_artists(credit: &str) -> Vec<String> {
    let flat = flattened_credit(credit);
    let mut artists = Vec::<String>::new();
    let mut rest = flat.as_str();
    loop {
        let lower = rest.to_ascii_lowercase();
        let next = ARTIST_SEPARATORS
            .iter()
            .filter_map(|separator| lower.find(separator).map(|at| (at, separator.len())))
            .min();
        let (artist, after) = match next {
            Some((at, length)) => {
                let (artist, after) = rest.split_at(at);
                (artist, Some(after.split_at(length).1))
            }
            None => (rest, None),
        };
        let artist = artist.trim();
        if !artist.is_empty()
            && !artists
                .iter()
                .any(|known| name_key(known) == name_key(artist))
        {
            artists.push(artist.to_owned());
        }
        match after {
            Some(after) => rest = after,
            None => break,
        }
    }
    artists
}

/// A credit with its brackets read: "(CV. Y)" and "(feat. Y)" become a further artist, and any
/// other bracket stays in the name.
fn flattened_credit(credit: &str) -> String {
    let mut flat = String::with_capacity(credit.len());
    let mut rest = credit;
    while let Some((before, inside)) = rest.split_once(['(', '（'])
        && let Some((group, after)) = inside.split_once([')', '）'])
    {
        flat.push_str(before);
        let lower = group.trim().nfkc().collect::<String>().to_ascii_lowercase();
        let voice = lower
            .strip_prefix("cv")
            .is_some_and(|voice| voice.starts_with(['.', ':', ' ']));
        if voice {
            // The voice follows "CV" and its mark, whatever their width.
            let name = group.trim().chars().skip(3).collect::<String>();
            flat.push_str(" / ");
            flat.push_str(name.trim());
        } else if ["feat. ", "feat ", "featuring ", "ft. ", "ft ", "with "]
            .iter()
            .any(|mark| lower.starts_with(mark))
        {
            flat.push(' ');
            flat.push_str(group.trim());
        } else {
            // The brackets stay as they were written.
            flat.extend(
                rest.strip_prefix(before)
                    .and_then(|open| open.chars().next()),
            );
            flat.push_str(group);
            flat.extend(
                inside
                    .strip_prefix(group)
                    .and_then(|close| close.chars().next()),
            );
        }
        rest = after;
    }
    flat.push_str(rest);
    flat
}

/// Words that mark a version where they stand alone after a title.
const VERSION_WORDS: &[&str] = &[
    "ver",
    "version",
    "remaster",
    "remastered",
    "live",
    "mix",
    "remix",
    "edit",
    "demo",
    "acoustic",
    "instrumental",
    "inst",
    "karaoke",
    "mono",
    "stereo",
    "bonus",
    "radio",
];

/// A title without what marks a version of it: trailing brackets such as "(Live)", "[2011
/// Remaster]" or "(feat. B)", a trailing "feat. B", and a trailing " - Remastered 2011". The
/// title itself when nothing else is left.
pub fn base_title(title: &str) -> &str {
    let mut base = title.trim();
    loop {
        let before = base;
        if let Some(close) = base.chars().next_back()
            && let Some(open) = match close {
                ')' | '）' => Some(['(', '（']),
                ']' | '］' => Some(['[', '［']),
                '】' => Some(['【', '【']),
                _ => None,
            }
            && let Some((head, _)) = base.rsplit_once(open)
        {
            base = head.trim_end();
        }
        let lower = base.to_ascii_lowercase();
        if let Some(at) = [" feat. ", " feat ", " featuring ", " ft. ", " ft "]
            .iter()
            .filter_map(|mark| lower.find(mark))
            .min()
        {
            base = base.split_at(at).0.trim_end();
        }
        if let Some((head, tail)) = base.rsplit_once(" - ")
            && tail
                .to_ascii_lowercase()
                .split(|character: char| !character.is_alphanumeric())
                .any(|word| VERSION_WORDS.contains(&word))
        {
            base = head.trim_end();
        }
        if base == before {
            break;
        }
    }
    if base.is_empty() { title.trim() } else { base }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_compare_by_their_letters() {
        for (left, right) in [
            ("Beyoncé", "BEYONCE"),
            ("ＧＯ☆Summer Girl", "go summer girl"),
            ("ｶﾞﾗｽ", "ガラス"),
            ("AC/DC", "ac dc"),
            ("Sigur Rós", "sigur ros"),
        ] {
            assert_eq!(name_key(left), name_key(right), "{left} and {right}");
        }
        // Kana keep their voicing marks, and a name of punctuation alone still compares.
        assert_ne!(name_key("かき"), name_key("がき"));
        assert_eq!(name_key(" & "), "&");
        assert_ne!(name_key("&"), name_key("+"));
    }

    #[test]
    fn a_credit_names_each_of_its_artists() {
        for (credit, artists) in [
            ("椎名林檎", &["椎名林檎"][..]),
            ("A feat. B & C", &["A", "B", "C"]),
            ("A (feat. B)", &["A", "B"]),
            (
                "Chihara Minori, Kitamura Eri",
                &["Chihara Minori", "Kitamura Eri"],
            ),
            ("レレイ(CV.東山奈央)", &["レレイ", "東山奈央"]),
            (
                "日高零奈(CV:蔀祐佳) & 東雲和音（CV：天音みほ）",
                &["日高零奈", "蔀祐佳", "東雲和音", "天音みほ"],
            ),
            (
                "Reiko Takahashi (高橋玲子)",
                &["Reiko Takahashi (高橋玲子)"],
            ),
            ("AC/DC", &["AC/DC"]),
            ("A、B", &["A", "B"]),
            ("A FEAT. a", &["A"]),
        ] {
            assert_eq!(credit_artists(credit), artists, "{credit}");
        }
    }

    #[test]
    fn a_title_loses_what_marks_its_version() {
        for (title, base) in [
            ("ショートショート (踊る様にver.)", "ショートショート"),
            ("SLUT ME OUT 2 (COUNTRY ME OUT)", "SLUT ME OUT 2"),
            ("Devil Inside (RJD2 Remix)", "Devil Inside"),
            ("Song feat. B", "Song"),
            ("Song (feat. B) [Live]", "Song"),
            ("Song - Remastered 2011", "Song"),
            ("Song - Alive", "Song - Alive"),
            (
                "(I Can't Get No) Satisfaction",
                "(I Can't Get No) Satisfaction",
            ),
            ("(Untitled)", "(Untitled)"),
            (
                "カンガルー ~カンガルーポケット~",
                "カンガルー ~カンガルーポケット~",
            ),
        ] {
            assert_eq!(base_title(title), base, "{title}");
        }
    }
}
