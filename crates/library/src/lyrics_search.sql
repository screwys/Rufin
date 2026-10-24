CREATE TABLE IF NOT EXISTS lyrics_documents (
    document_key INTEGER PRIMARY KEY,
    media_uri TEXT NOT NULL,
    authority TEXT NOT NULL,
    role TEXT NOT NULL,
    language TEXT NOT NULL,
    script TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    text TEXT NOT NULL,
    UNIQUE(media_uri, authority, role, language, script, ordinal),
    FOREIGN KEY(media_uri, authority, role, language, script)
        REFERENCES lyrics_cache(media_uri, authority, role, language, script)
        ON DELETE CASCADE ON UPDATE CASCADE
) STRICT;

-- Only the display text of each version-4 lyric document is searchable.
CREATE VIEW IF NOT EXISTS lyrics_search_input AS
SELECT cache.rowid cache_rowid, cache.media_uri, cache.authority, cache.role,
    cache.language, cache.script, CAST(document.key AS INTEGER) ordinal,
    (SELECT group_concat(text, char(10)) FROM (
        SELECT replace(replace(json_extract(line.value, '$.text'), char(1), ''), char(2), '') text
        FROM json_each(document.value, '$.lines') line
        ORDER BY CAST(line.key AS INTEGER)
    )) text
FROM lyrics_cache cache,
    json_each(CASE WHEN json_valid(cache.lyrics) THEN
        CASE WHEN json_extract(cache.lyrics, '$.version') = 4
            THEN json_extract(cache.lyrics, '$.bundle.content.Documents') END
        END) document;

CREATE VIRTUAL TABLE IF NOT EXISTS lyrics_fts USING fts5(
    text, content='lyrics_documents', content_rowid='document_key',
    tokenize='rufin_lyrics', prefix='3'
);

CREATE TRIGGER IF NOT EXISTS lyrics_documents_insert AFTER INSERT ON lyrics_documents BEGIN
    INSERT INTO lyrics_fts(rowid, text) VALUES(new.document_key, new.text);
END;
CREATE TRIGGER IF NOT EXISTS lyrics_documents_delete AFTER DELETE ON lyrics_documents BEGIN
    INSERT INTO lyrics_fts(lyrics_fts, rowid, text) VALUES('delete', old.document_key, old.text);
END;
CREATE TRIGGER IF NOT EXISTS lyrics_documents_update AFTER UPDATE OF text ON lyrics_documents BEGIN
    INSERT INTO lyrics_fts(lyrics_fts, rowid, text) VALUES('delete', old.document_key, old.text);
    INSERT INTO lyrics_fts(rowid, text) VALUES(new.document_key, new.text);
END;

CREATE TRIGGER IF NOT EXISTS lyrics_search_insert AFTER INSERT ON lyrics_cache BEGIN
    INSERT INTO lyrics_documents(media_uri,authority,role,language,script,ordinal,text)
        SELECT media_uri,authority,role,language,script,ordinal,text FROM lyrics_search_input
        WHERE cache_rowid=new.rowid AND length(trim(text))>0;
END;
CREATE TRIGGER IF NOT EXISTS lyrics_search_update AFTER UPDATE OF lyrics ON lyrics_cache
WHEN old.lyrics IS NOT new.lyrics BEGIN
    DELETE FROM lyrics_documents WHERE media_uri=new.media_uri AND authority=new.authority
        AND role=new.role AND language=new.language AND script=new.script;
    INSERT INTO lyrics_documents(media_uri,authority,role,language,script,ordinal,text)
        SELECT media_uri,authority,role,language,script,ordinal,text FROM lyrics_search_input
        WHERE cache_rowid=new.rowid AND length(trim(text))>0;
END;
