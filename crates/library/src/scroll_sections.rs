use sqlx::{Execute, QueryBuilder, Sqlite, SqliteConnection};

use crate::LibraryResult;

#[derive(Clone, Debug, serde::Deserialize)]
pub struct ScrollSection {
    pub title: String,
    pub index: u64,
}

// This projection wraps the existing filtered query and its exact ordering. Only
// the first position for each compact writing-system bucket leaves SQLite.
pub(crate) fn section_sql(sql: &str, text: &str) -> String {
    let mut depth = 0;
    let mut quoted = false;
    let mut select = 0;
    let mut from = 0;
    let mut order = 0;
    let mut limit = sql.len();
    let bytes = sql.as_bytes();
    for index in 0..bytes.len() {
        match bytes[index] {
            b'\'' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => depth -= 1,
            _ => {}
        }
        if quoted || depth != 0 {
            continue;
        }
        if bytes[index..].starts_with(b"SELECT ") {
            select = index;
        }
        if bytes[index..].starts_with(b"FROM ") {
            from = index;
        }
        if bytes[index..].starts_with(b"ORDER BY") {
            order = index;
        }
        if bytes[index..].starts_with(b"LIMIT ") {
            limit = index;
        }
    }
    let mut order_start = order + 8;
    while order_start < limit && bytes[order_start].is_ascii_whitespace() {
        order_start += 1;
    }
    let mut first_end = limit;
    depth = 0;
    for (index, byte) in bytes.iter().enumerate().take(limit).skip(order_start) {
        match byte {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                first_end = index;
                break;
            }
            _ => {}
        }
    }
    let text = if text.is_empty() {
        order_text(
            sql.get(order_start..first_end)
                .expect("SQL order boundaries")
                .trim(),
        )
    } else {
        text
    };
    let input = format!(
        "{}SELECT {text} section_text,row_number() OVER (ORDER BY {})-1 section_position {} {}",
        sql.get(..select).expect("SQL SELECT boundary"),
        sql.get(order_start..limit).expect("SQL order boundaries"),
        sql.get(from..order).expect("SQL FROM boundaries"),
        sql.get(limit..).expect("SQL LIMIT boundary")
    );
    format!("WITH section_rows AS ({input}), initials AS (SELECT lower(substr(section_text,1,1)) initial,unicode(substr(section_text,1,1)) code,section_position FROM section_rows), buckets AS (SELECT {SECTION_BUCKET_SQL} title,section_position FROM initials)
        SELECT json_object('title',title,'index',min(section_position)) FROM buckets GROUP BY title ORDER BY min(section_position)")
}

pub(crate) const SECTION_BUCKET_SQL: &str = "CASE
        WHEN initial BETWEEN 'a' AND 'z' THEN upper(initial)
        WHEN code BETWEEN 65382 AND 65437 AND code<>65392 THEN CASE
            WHEN code=65382 OR code>=65436 THEN 'わ'
            WHEN code BETWEEN 65383 AND 65387 OR code BETWEEN 65393 AND 65397 THEN 'あ'
            WHEN code BETWEEN 65398 AND 65402 THEN 'か'
            WHEN code BETWEEN 65403 AND 65407 THEN 'さ'
            WHEN code=65391 OR code BETWEEN 65408 AND 65412 THEN 'た'
            WHEN code BETWEEN 65413 AND 65417 THEN 'な'
            WHEN code BETWEEN 65418 AND 65422 THEN 'は'
            WHEN code BETWEEN 65423 AND 65427 THEN 'ま'
            WHEN code BETWEEN 65388 AND 65390 OR code BETWEEN 65428 AND 65430 THEN 'や'
            ELSE 'ら' END
        WHEN code BETWEEN 12353 AND 12438 OR code BETWEEN 12449 AND 12538 THEN CASE
            WHEN (CASE WHEN code>=12449 THEN code-96 ELSE code END)=12436 THEN 'あ'
            WHEN (CASE WHEN code>=12449 THEN code-96 ELSE code END) IN (12437,12438) THEN 'か'
            WHEN (CASE WHEN code>=12449 THEN code-96 ELSE code END)<=12362 THEN 'あ'
            WHEN (CASE WHEN code>=12449 THEN code-96 ELSE code END)<=12372 THEN 'か'
            WHEN (CASE WHEN code>=12449 THEN code-96 ELSE code END)<=12382 THEN 'さ'
            WHEN (CASE WHEN code>=12449 THEN code-96 ELSE code END)<=12393 THEN 'た'
            WHEN (CASE WHEN code>=12449 THEN code-96 ELSE code END)<=12398 THEN 'な'
            WHEN (CASE WHEN code>=12449 THEN code-96 ELSE code END)<=12413 THEN 'は'
            WHEN (CASE WHEN code>=12449 THEN code-96 ELSE code END)<=12418 THEN 'ま'
            WHEN (CASE WHEN code>=12449 THEN code-96 ELSE code END)<=12424 THEN 'や'
            WHEN (CASE WHEN code>=12449 THEN code-96 ELSE code END)<=12429 THEN 'ら'
            ELSE 'わ' END
        WHEN code BETWEEN 13312 AND 19903 OR code BETWEEN 19968 AND 40959 OR code BETWEEN 63744 AND 64255 OR code BETWEEN 131072 AND 205743 THEN '漢'
        WHEN code BETWEEN 44032 AND 55203 OR code BETWEEN 4352 AND 4607 OR code BETWEEN 12593 AND 12686 THEN '한'
        WHEN initial BETWEEN '0' AND '9' OR code<128 OR code IS NULL THEN '#'
        ELSE '…' END";

pub(crate) fn decode_sections(rows: Vec<String>) -> LibraryResult<Vec<ScrollSection>> {
    rows.into_iter()
        .map(|row| serde_json::from_str(&row).map_err(Into::into))
        .collect()
}

pub(crate) async fn builder_sections(
    mut query: QueryBuilder<Sqlite>,
    text: &str,
    connection: &mut SqliteConnection,
) -> LibraryResult<Vec<ScrollSection>> {
    let sql = section_sql(query.sql().as_str(), text);
    let arguments = query
        .build()
        .take_arguments()
        .map_err(sqlx::Error::Encode)?
        .unwrap_or_default();
    decode_sections(
        sqlx::query_scalar_with(sqlx::AssertSqlSafe(sql), arguments)
            .persistent(false)
            .fetch_all(connection)
            .await?,
    )
}

pub(crate) async fn builder_rows<T>(
    mut query: QueryBuilder<Sqlite>,
    sections: bool,
    connection: &mut SqliteConnection,
) -> LibraryResult<Vec<T>>
where
    for<'r> T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite> + Send + Unpin,
{
    let sql = if sections {
        section_sql(query.sql().as_str(), "")
    } else {
        query.sql().as_str().to_string()
    };
    let arguments = query
        .build()
        .take_arguments()
        .map_err(sqlx::Error::Encode)?
        .unwrap_or_default();
    Ok(sqlx::query_scalar_with(sqlx::AssertSqlSafe(sql), arguments)
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

pub(crate) fn text_sort(sort: crate::TrackSort) -> bool {
    matches!(
        sort,
        crate::TrackSort::Title
            | crate::TrackSort::Artist
            | crate::TrackSort::AlbumArtist
            | crate::TrackSort::Album
            | crate::TrackSort::Genre
    )
}

pub(crate) fn order_text(order: &str) -> &str {
    let order = order.strip_suffix(" NULLS LAST").unwrap_or(order);
    order
        .strip_suffix(" ASC")
        .or_else(|| order.strip_suffix(" DESC"))
        .unwrap_or(order)
}
