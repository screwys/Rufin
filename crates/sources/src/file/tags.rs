//! The tags a scan reads. A file's values are kept under its format's own keys, lowercase; one
//! alias table says which keys name which tag and how its values split. The tags and their
//! aliases are Navidrome's `resources/mappings.yaml`; genres and moods split as Rufin's always
//! have. A cleaned tag holds each value once, trimmed, bounded and in its type.

use std::collections::BTreeMap;

/// A file's tag values by its format's own key, lowercase.
pub(crate) type RawTags = BTreeMap<String, Vec<String>>;

/// Adds one value of a format's key.
pub(crate) fn push(raw: &mut RawTags, key: &str, value: &str) {
    let value =
        value.trim_matches(|character: char| character.is_whitespace() || character == '\0');
    if !value.is_empty() {
        raw.entry(key.to_lowercase())
            .or_default()
            .push(value.to_owned());
    }
}

/// What parts the names of one credit value: `;`, a spaced ` / ` so that a name keeps its own
/// slash, as AC/DC does, and the words that introduce a featured artist.
pub(crate) const ARTIST_SEPARATORS: [&str; 6] = [";", " / ", " feat. ", " feat ", " ft. ", " ft "];

/// The parts of `value` between `separators`, whatever their case, each trimmed.
pub(crate) fn parts<'a>(value: &'a str, separators: &[&str]) -> Vec<&'a str> {
    let bytes = value.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    for (at, _) in value.char_indices() {
        // Inside the separator that ended the last part.
        if at < start {
            continue;
        }
        let rest = &bytes[at..];
        let separator = separators.iter().find(|separator| {
            rest.len() >= separator.len()
                && rest[..separator.len()].eq_ignore_ascii_case(separator.as_bytes())
        });
        if let Some(separator) = separator {
            parts.push(value.get(start..at).unwrap_or_default().trim());
            start = at + separator.len();
        }
    }
    parts.push(value.get(start..).unwrap_or_default().trim());
    parts
}

/// The parts of `value` that are not empty.
pub(crate) fn split(value: &str, separators: &[&str]) -> Vec<String> {
    parts(value, separators)
        .into_iter()
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A MusicBrainz id as tags hold one.
pub(crate) fn clean_mbid(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
    .then(|| value.to_string())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Text,
    Date,
    Uuid,
}

struct Mapping {
    name: &'static str,
    aliases: &'static [&'static str],
    split: &'static [&'static str],
    kind: Kind,
    max_length: usize,
}

/// Navidrome's tags and aliases: its main tags, then its additional ones, with the few keys
/// the editor writes that it lacks. Genres and moods split as Rufin's always have, without
/// the comma; a bare year is its own tag, the last a date is read from. Lyrics are read by
/// `lyrics`, so they have no row.
#[rustfmt::skip]
const MAPPINGS: &[Mapping] = &[
    Mapping { name: "title", aliases: &["tit2", "title", "©nam", "inam"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "titlesort", aliases: &["tsot", "titlesort", "sonm", "wm/titlesortorder"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "artist", aliases: &["tpe1", "artist", "©art", "author", "iart"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "artistsort", aliases: &["tsop", "artistsort", "soar", "wm/artistsortorder"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "artists", aliases: &["txxx:artists", "artists", "----:com.apple.itunes:artists", "wm/artists"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "artistssort", aliases: &["artistssort"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "arranger", aliases: &["tipl:arranger", "ipls:arranger", "arranger"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "composer", aliases: &["tcom", "composer", "©wrt", "wm/composer", "imus", "writer", "txxx:writer", "iwri"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "composersort", aliases: &["tsoc", "txxx:composersort", "composersort", "soco", "wm/composersortorder"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "lyricist", aliases: &["text", "lyricist", "----:com.apple.itunes:lyricist", "wm/writer"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "lyricistsort", aliases: &["lyricistsort"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "conductor", aliases: &["tpe3", "conductor", "----:com.apple.itunes:conductor", "wm/conductor"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "director", aliases: &["txxx:director", "director", "©dir", "wm/director"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "djmixer", aliases: &["tipl:dj-mix", "ipls:dj-mix", "djmixer", "----:com.apple.itunes:djmixer", "wm/djmixer"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "mixer", aliases: &["tipl:mix", "ipls:mix", "mixer", "----:com.apple.itunes:mixer", "wm/mixer"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "engineer", aliases: &["tipl:engineer", "ipls:engineer", "engineer", "----:com.apple.itunes:engineer", "wm/engineer", "ieng"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "producer", aliases: &["tipl:producer", "ipls:producer", "producer", "----:com.apple.itunes:producer", "wm/producer", "ipro"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "remixer", aliases: &["tpe4", "remixer", "mixartist", "----:com.apple.itunes:remixer", "wm/modifiedby"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "albumartist", aliases: &["tpe2", "albumartist", "album artist", "album_artist", "aart", "wm/albumartist"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "albumartistsort", aliases: &["tso2", "txxx:albumartistsort", "albumartistsort", "soaa", "wm/albumartistsortorder"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "albumartists", aliases: &["txxx:album artists", "albumartists"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "albumartistssort", aliases: &["albumartistssort"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "album", aliases: &["talb", "album", "©alb", "wm/albumtitle", "iprd"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "albumsort", aliases: &["tsoa", "albumsort", "soal", "wm/albumsortorder"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "albumversion", aliases: &["albumversion", "musicbrainz_albumcomment", "musicbrainz album comment"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "genre", aliases: &["tcon", "genre", "©gen", "wm/genre", "ignr"], split: &[";", "/"], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "mood", aliases: &["tmoo", "mood", "----:com.apple.itunes:mood", "wm/mood"], split: &[";", "/"], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "compilation", aliases: &["tcmp", "compilation", "cpil", "wm/iscompilation"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "track", aliases: &["track", "trck", "tracknumber", "trkn", "wm/tracknumber", "itrk", "iprt"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "tracktotal", aliases: &["tracktotal", "totaltracks", "ifrm"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "disc", aliases: &["tpos", "disc", "discnumber", "disk", "wm/partofset"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "disctotal", aliases: &["disctotal", "totaldiscs"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "discsubtitle", aliases: &["tsst", "discsubtitle", "----:com.apple.itunes:discsubtitle", "setsubtitle", "wm/setsubtitle"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "bpm", aliases: &["tbpm", "bpm", "tmpo", "----:com.apple.itunes:bpm", "wm/beatsperminute"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "comment", aliases: &["comm:description", "comment", "©cmt", "description", "icmt"], split: &[], kind: Kind::Text, max_length: 4096 },
    Mapping { name: "originaldate", aliases: &["tdor", "originaldate", "----:com.apple.itunes:originaldate", "wm/originalreleasetime", "tory", "originalyear", "----:com.apple.itunes:originalyear", "wm/originalreleaseyear", "origyear", "----:com.apple.itunes:origyear"], split: &[], kind: Kind::Date, max_length: 1024 },
    Mapping { name: "recordingdate", aliases: &["tdrc", "date", "recordingdate", "icrd", "record date"], split: &[], kind: Kind::Date, max_length: 1024 },
    Mapping { name: "releasedate", aliases: &["tdrl", "releasedate", "©day", "----:com.apple.itunes:releasedate"], split: &[], kind: Kind::Date, max_length: 1024 },
    Mapping { name: "year", aliases: &["year", "wm/year"], split: &[], kind: Kind::Date, max_length: 1024 },
    Mapping { name: "catalognumber", aliases: &["txxx:catalognumber", "catalognumber", "----:com.apple.itunes:catalognumber", "wm/catalogno"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "musicbrainz_artistid", aliases: &["txxx:musicbrainz artist id", "musicbrainz_artistid", "musicbrainz artist id", "----:com.apple.itunes:musicbrainz artist id", "musicbrainz/artist id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_recordingid", aliases: &["ufid:http://musicbrainz.org", "musicbrainz_trackid", "musicbrainz track id", "----:com.apple.itunes:musicbrainz track id", "musicbrainz/track id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_trackid", aliases: &["txxx:musicbrainz release track id", "musicbrainz_releasetrackid", "----:com.apple.itunes:musicbrainz release track id", "musicbrainz/release track id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_albumartistid", aliases: &["txxx:musicbrainz album artist id", "musicbrainz_albumartistid", "musicbrainz album artist id", "----:com.apple.itunes:musicbrainz album artist id", "musicbrainz/album artist id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_albumid", aliases: &["txxx:musicbrainz album id", "musicbrainz_albumid", "musicbrainz album id", "----:com.apple.itunes:musicbrainz album id", "musicbrainz/album id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_releasegroupid", aliases: &["txxx:musicbrainz release group id", "musicbrainz_releasegroupid", "----:com.apple.itunes:musicbrainz release group id", "musicbrainz/release group id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_composerid", aliases: &["txxx:musicbrainz composer id", "musicbrainz_composerid", "musicbrainz_composer_id", "----:com.apple.itunes:musicbrainz composer id", "musicbrainz/composer id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_lyricistid", aliases: &["txxx:musicbrainz lyricist id", "musicbrainz_lyricistid", "musicbrainz_lyricist_id", "----:com.apple.itunes:musicbrainz lyricist id", "musicbrainz/lyricist id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_directorid", aliases: &["txxx:musicbrainz director id", "musicbrainz_directorid", "musicbrainz_director_id", "----:com.apple.itunes:musicbrainz director id", "musicbrainz/director id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_producerid", aliases: &["txxx:musicbrainz producer id", "musicbrainz_producerid", "musicbrainz_producer_id", "----:com.apple.itunes:musicbrainz producer id", "musicbrainz/producer id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_engineerid", aliases: &["txxx:musicbrainz engineer id", "musicbrainz_engineerid", "musicbrainz_engineer_id", "----:com.apple.itunes:musicbrainz engineer id", "musicbrainz/engineer id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_mixerid", aliases: &["txxx:musicbrainz mixer id", "musicbrainz_mixerid", "musicbrainz_mixer_id", "----:com.apple.itunes:musicbrainz mixer id", "musicbrainz/mixer id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_remixerid", aliases: &["txxx:musicbrainz remixer id", "musicbrainz_remixerid", "musicbrainz_remixer_id", "----:com.apple.itunes:musicbrainz remixer id", "musicbrainz/remixer id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_djmixerid", aliases: &["txxx:musicbrainz djmixer id", "musicbrainz_djmixerid", "musicbrainz_djmixer_id", "----:com.apple.itunes:musicbrainz djmixer id", "musicbrainz/djmixer id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_conductorid", aliases: &["txxx:musicbrainz conductor id", "musicbrainz_conductorid", "musicbrainz_conductor_id", "----:com.apple.itunes:musicbrainz conductor id", "musicbrainz/conductor id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_arrangerid", aliases: &["txxx:musicbrainz arranger id", "musicbrainz_arrangerid", "musicbrainz_arranger_id", "----:com.apple.itunes:musicbrainz arranger id", "musicbrainz/arranger id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "releasetype", aliases: &["txxx:musicbrainz album type", "releasetype", "musicbrainz_albumtype", "----:com.apple.itunes:musicbrainz album type", "musicbrainz/album type"], split: &[","], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "replaygain_album_gain", aliases: &["txxx:replaygain_album_gain", "replaygain_album_gain", "----:com.apple.itunes:replaygain_album_gain"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "replaygain_album_peak", aliases: &["txxx:replaygain_album_peak", "replaygain_album_peak", "----:com.apple.itunes:replaygain_album_peak"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "replaygain_track_gain", aliases: &["txxx:replaygain_track_gain", "replaygain_track_gain", "----:com.apple.itunes:replaygain_track_gain"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "replaygain_track_peak", aliases: &["txxx:replaygain_track_peak", "replaygain_track_peak", "----:com.apple.itunes:replaygain_track_peak"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "r128_album_gain", aliases: &["r128_album_gain"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "r128_track_gain", aliases: &["r128_track_gain"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "performer", aliases: &["performer"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "musicbrainz_performerid", aliases: &["txxx:musicbrainz performer id", "musicbrainz_performerid", "musicbrainz_performer_id", "----:com.apple.itunes:musicbrainz performer id", "musicbrainz/performer id"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "explicitstatus", aliases: &["itunesadvisory", "rtng"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "asin", aliases: &["txxx:asin", "asin", "----:com.apple.itunes:asin"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "barcode", aliases: &["txxx:barcode", "barcode", "----:com.apple.itunes:barcode", "wm/barcode"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "copyright", aliases: &["tcop", "copyright", "cprt", "icop"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "encodedby", aliases: &["tenc", "encodedby", "©too", "wm/encodedby", "ienc"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "encodersettings", aliases: &["tsse", "encodersettings", "----:com.apple.itunes:encodersettings", "wm/encodingsettings"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "grouping", aliases: &["grp1", "grouping", "©grp", "wm/contentgroupdescription"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "key", aliases: &["tkey", "key", "initialkey", "----:com.apple.itunes:initialkey", "wm/initialkey"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "isrc", aliases: &["tsrc", "isrc", "----:com.apple.itunes:isrc", "wm/isrc"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "language", aliases: &["tlan", "language", "----:com.apple.itunes:language", "wm/language", "ilng"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "license", aliases: &["wcop", "txxx:license", "license", "----:com.apple.itunes:license"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "media", aliases: &["tmed", "media", "----:com.apple.itunes:media", "wm/media", "imed"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "movementname", aliases: &["mvnm", "movementname", "©mvn"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "movementtotal", aliases: &["movementtotal", "movementcount", "mvc"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "movement", aliases: &["mvin", "movement", "movementnumber", "mvi"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "recordlabel", aliases: &["tpub", "label", "publisher", "----:com.apple.itunes:label", "wm/publisher", "organization"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "musicbrainz_discid", aliases: &["txxx:musicbrainz disc id", "musicbrainz_discid", "musicbrainz disc id", "----:com.apple.itunes:musicbrainz disc id", "musicbrainz/disc id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "musicbrainz_workid", aliases: &["txxx:musicbrainz work id", "musicbrainz_workid", "musicbrainz work id", "----:com.apple.itunes:musicbrainz work id", "musicbrainz/work id"], split: &[], kind: Kind::Uuid, max_length: 1024 },
    Mapping { name: "releasecountry", aliases: &["txxx:musicbrainz album release country", "releasecountry", "----:com.apple.itunes:musicbrainz album release country", "musicbrainz/album release country", "icnt"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "releasestatus", aliases: &["txxx:musicbrainz album status", "releasestatus", "musicbrainz_albumstatus", "----:com.apple.itunes:musicbrainz album status", "musicbrainz/album status"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "script", aliases: &["txxx:script", "script", "----:com.apple.itunes:script", "wm/script"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "subtitle", aliases: &["tit3", "subtitle", "----:com.apple.itunes:subtitle", "wm/subtitle"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "website", aliases: &["woar", "website", "weblink", "wm/authorurl"], split: &[], kind: Kind::Text, max_length: 1024 },
    Mapping { name: "work", aliases: &["txxx:work", "tit1", "work", "©wrk", "wm/work"], split: &[], kind: Kind::Text, max_length: 1024 },
];

/// The cleaned tags of one file, by the table's tag name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Tags(BTreeMap<&'static str, Vec<String>>);

impl Tags {
    pub(crate) fn clean(raw: &RawTags) -> Self {
        let mut tags = BTreeMap::new();
        for mapping in MAPPINGS {
            let mut values = Vec::<String>::new();
            // The aliases are one tag's names in other formats and taggers, not further values:
            // the first a file carries is its tag.
            for value in mapping
                .aliases
                .iter()
                .find_map(|alias| raw.get(*alias))
                .into_iter()
                .flatten()
            {
                // Taggers join several ids in one value.
                let parts = match (mapping.kind, mapping.split) {
                    (Kind::Uuid, _) => split(value, &[";", "/"]),
                    (_, []) => vec![value.trim().to_owned()],
                    (_, separators) => split(value, separators),
                };
                for mut part in parts {
                    if part.len() > mapping.max_length {
                        let mut end = mapping.max_length;
                        while !part.is_char_boundary(end) {
                            end -= 1;
                        }
                        part.truncate(end);
                    }
                    let part = match mapping.kind {
                        Kind::Text => Some(part),
                        Kind::Date => library::normalized_date(Some(part)),
                        Kind::Uuid => clean_mbid(&part),
                    };
                    if let Some(part) = part.filter(|part| !part.is_empty())
                        && !values.contains(&part)
                    {
                        values.push(part);
                    }
                }
            }
            if !values.is_empty() {
                tags.insert(mapping.name, values);
            }
        }
        Self(tags)
    }

    /// A tag's values in the file's order.
    pub(crate) fn all(&self, name: &str) -> &[String] {
        self.0.get(name).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn first(&self, name: &str) -> Option<&str> {
        self.all(name).first().map(String::as_str)
    }

    /// The tags as a track keeps them.
    pub(crate) fn stored(&self) -> BTreeMap<String, Vec<String>> {
        self.0
            .iter()
            .map(|(name, values)| ((*name).to_owned(), values.clone()))
            .collect()
    }
}

/// The tag a format's key names.
#[cfg(test)]
pub(crate) fn named(key: &str) -> Option<&'static str> {
    let key = key.to_lowercase();
    MAPPINGS
        .iter()
        .find(|mapping| mapping.aliases.contains(&key.as_str()))
        .map(|mapping| mapping.name)
}
