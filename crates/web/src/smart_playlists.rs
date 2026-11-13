//! The smart playlist editor: its choices, the values a rule suggests, the live track count
//! and the save, as GTK's editor reads them from core.

use super::*;
use library::{
    ReadCancellation, SmartPlaylistActivityPeriod as Period, SmartPlaylistCondition,
    SmartPlaylistDefinition, SmartPlaylistKey, SmartPlaylistMatch, SmartPlaylistRuleField,
    SmartPlaylistSort,
};
use localization::tr;
use rufin_core::playlists::SmartPlaylistChange;
use rufin_core::settings::presentation as titles;

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api/smart-playlists/editor", get(editor))
        .route("/api/smart-playlists/values", get(values))
        .route("/api/smart-playlists/count", post(count))
}

/// What the editor offers, translated, and the name and definition of the playlist `key`
/// names, or a new playlist's.
async fn editor(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let (editing, name, definition) = match parameters.get("key") {
        Some(_) => {
            let smart: SmartPlaylistKey = key(&parameters, "key")?;
            let rows = products
                .library
                .rows_of(
                    &[library::Item::Smart(smart)],
                    false,
                    &ReadCancellation::new(),
                )
                .await
                .map_err(internal)?;
            match rows.into_iter().next() {
                Some(library::Row::Playlist(library::PlaylistListRow::SmartPlaylist(row))) => {
                    (Some(smart), row.name, row.definition)
                }
                _ => return Err(error(StatusCode::NOT_FOUND, "Smart Playlist not found")),
            }
        }
        None => (None, String::new(), SmartPlaylistDefinition::default()),
    };
    let reads_plays = |conditions, sort_field| {
        SmartPlaylistDefinition {
            conditions,
            sort_field,
            ..SmartPlaylistDefinition::default()
        }
        .reads_plays()
    };
    let fields = SmartPlaylistRuleField::ALL.map(|field| {
        use SmartPlaylistRuleField as F;
        let operators = field
            .operators()
            .iter()
            .filter_map(|&operator| {
                let kind = field.value_kind(operator)?;
                let (min, max, _) = field.number_bounds(operator);
                Some(json!({
                    "operator": operator,
                    "title": tr(titles::smart_operator_title(field, operator)),
                    "kind": format!("{kind:?}"),
                    "min": min,
                    "max": max,
                    "default": field.default_value(operator),
                }))
            })
            .collect::<Vec<_>>();
        json!({
            "field": field,
            "title": tr(titles::smart_rule_field_title(field)),
            "section": tr(titles::smart_field_section_title(field.section())),
            "placeholder": tr(titles::smart_value_placeholder(field)),
            "scale": titles::smart_number_scale(field),
            "suggests": matches!(field, F::Artist | F::Album | F::Genre | F::Mood),
            "reads_plays": reads_plays(
                vec![SmartPlaylistCondition::Rule(field.default_rule())],
                SmartPlaylistSort::Title,
            ),
            "rule": field.default_rule(),
            "operators": operators,
        })
    });
    let sorts = SmartPlaylistSort::ALL.map(|sort| {
        json!({"sort":sort,"title":tr(titles::smart_sort_title(sort)),"reads_plays":reads_plays(Vec::new(), sort)})
    });
    let periods = Period::ALL
        .map(|period| json!({"period":period,"title":tr(titles::smart_period_title(period))}));
    let matches = [SmartPlaylistMatch::All, SmartPlaylistMatch::Any].map(
        |matching| json!({"matching":matching,"title":tr(titles::smart_match_title(matching))}),
    );
    let sources = products
        .source
        .list_sources()
        .sources
        .iter()
        .map(|source| json!({"id":source.id,"name":source.name}))
        .collect::<Vec<_>>();
    let templates = rufin_core::playlists::smart_playlist_templates()
        .into_iter()
        .map(|template| json!({"name":tr(template.name),"definition":template.definition}))
        .collect::<Vec<_>>();
    Ok(json_response(
        StatusCode::OK,
        json!({
            "key": editing,
            "name": name,
            "definition": definition,
            "fields": fields,
            "sorts": sorts,
            "periods": periods,
            "matches": matches,
            "sources": sources,
            "templates": templates,
        }),
    ))
}

/// The library a request reads in: `source` and `folder`, as a page names them.
async fn selected(
    products: &ProductHandles,
    source: Option<&str>,
    folder: Option<&str>,
) -> Result<Option<rufin_core::runtime::SelectedLibrary>, Error> {
    let source = source.map(sources::SourceId::new);
    page::scope(products, source.as_ref(), folder).await
}

/// The values of `field` whose names hold `q`, from the playlist's source `from`.
async fn values(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let field: SmartPlaylistRuleField =
        serde_json::from_value(json!(required(&parameters, "field")?)).map_err(bad_request)?;
    let selection = selected(
        &products,
        parameters.get("source").map(String::as_str),
        parameters.get("folder").map(String::as_str),
    )
    .await?;
    let from = parameters.get("from").map(sources::SourceId::new);
    let values = match selection {
        Some(selection) => rufin_core::playlists::smart_playlist_values(
            &products.library,
            &selection,
            field,
            parameters.get("q").map_or("", String::as_str),
            from.as_ref(),
        )
        .await
        .map_err(internal)?,
        None => Vec::new(),
    };
    let values = values
        .iter()
        .map(|choice| json!({"value":choice.value,"name":choice.name,"tracks":choice.tracks}))
        .collect::<Vec<_>>();
    Ok(json_response(StatusCode::OK, json!({"values": values})))
}

#[derive(Deserialize)]
struct Counted {
    definition: SmartPlaylistDefinition,
    source: Option<String>,
    folder: Option<String>,
}

/// How many tracks a definition being edited matches now, and the header's text for it.
async fn count(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let input: Counted = body(request).await?;
    let selection = selected(&products, input.source.as_deref(), input.folder.as_deref()).await?;
    let count = match selection {
        Some(selection) => rufin_core::playlists::smart_playlist_match_count(
            &products.library,
            &selection,
            &input.definition,
        )
        .await
        .map_err(internal)?,
        None => 0,
    };
    Ok(json_response(
        StatusCode::OK,
        json!({"count":count,"text":localization::track_count_text(count)}),
    ))
}

/// Creates a smart playlist, in the playlist folder `folder` when one is given, or saves the
/// one `key` names.
pub(super) async fn save(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        key: Option<SmartPlaylistKey>,
        name: String,
        definition: SmartPlaylistDefinition,
        folder: Option<library::PlaylistFolderKey>,
    }
    let input: Input = body(request).await?;
    let change = match input.key {
        Some(key) => SmartPlaylistChange::Update {
            key,
            name: input.name,
            definition: input.definition,
            artwork: None,
        },
        None => SmartPlaylistChange::Create {
            name: input.name,
            definition: input.definition,
        },
    };
    let saved =
        rufin_core::playlists::apply_smart_playlist_change(&products.source, change, input.folder)
            .await
            .map_err(bad_request)?
            .ok_or_else(|| error(StatusCode::NOT_FOUND, "Smart Playlist not found"))?;
    Ok(json_response(StatusCode::OK, json!({"id": saved})))
}
