use super::*;
use crate::runtime::source::*;

pub(super) async fn handle(
    request: Request<Incoming>,
    products: &ProductHandles,
    parameters: &HashMap<String, String>,
) -> Result<Response<Body>, Error> {
    let path = request.uri().path().to_owned();
    let source = &products.source;
    match (request.method(), path.as_str()) {
        (&Method::GET, "/api/sources") => {
            let configured = source.list_sources();
            Ok(json_response(
                StatusCode::OK,
                json!({
                    "selected": configured.selected_source_id,
                    "sources": configured.sources.iter().map(|s| json!({"id":s.id,"name":s.name,"kind":s.kind})).collect::<Vec<_>>()
                }),
            ))
        }
        (&Method::GET, "/api/sources/progress") => Ok(json_response(
            StatusCode::OK,
            serde_json::to_value(&*source.operation().borrow()).map_err(internal)?,
        )),
        (&Method::GET, "/api/sources/events") => {
            let stream = futures_util::stream::unfold(
                (source.operation(), true),
                |(mut operation, first)| async move {
                    if !first && operation.changed().await.is_err() {
                        return None;
                    }
                    let value = serde_json::to_value(&*operation.borrow_and_update())
                        .expect("source operation serialization");
                    Some((
                        Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                            "event: source\ndata: {value}\n\n"
                        )))),
                        (operation, false),
                    ))
                },
            );
            Ok(event_response(BodyExt::boxed(StreamBody::new(stream))))
        }
        (&Method::POST, "/api/sources") | (&Method::POST, "/api/sources/local") => {
            let setup = if path.ends_with("/local") {
                #[derive(Deserialize)]
                struct Input {
                    paths: Vec<PathBuf>,
                }
                SourceSetup::Local {
                    roots: body::<Input>(request).await?.paths,
                }
            } else {
                body::<SourceSetup>(request).await?
            };
            let selected = completion(source.configure_source(setup)).await?;
            Ok(json_response(
                StatusCode::OK,
                json!({"id":selected.source_id}),
            ))
        }
        (&Method::PATCH, "/api/sources") => {
            completion(source.update_source(body(request).await?)).await?;
            Ok(json_response(StatusCode::OK, json!({"updated":true})))
        }
        (&Method::POST, "/api/sources/select") | (&Method::POST, "/api/sources/refresh") => {
            let input: Identifier = body(request).await?;
            if input.id.is_empty() {
                return Err(bad_request("id is required"));
            }
            let id = sources::SourceId::new(input.id);
            if path.ends_with("/refresh") {
                completion(source.refresh_source(id)).await?;
                Ok(json_response(StatusCode::OK, json!({"refreshed":true})))
            } else {
                let selected = completion(source.select_source(id)).await?;
                Ok(json_response(
                    StatusCode::OK,
                    json!({"id":selected.source_id}),
                ))
            }
        }
        (&Method::DELETE, "/api/sources") => {
            completion(source.forget_source(sources::SourceId::new(required(parameters, "id")?)))
                .await?;
            Ok(json_response(StatusCode::OK, json!({"removed":true})))
        }
        (&Method::POST, "/api/sources/login/plex") => {
            Ok(login_response(source.plex_login(body(request).await?)))
        }
        (&Method::POST, "/api/sources/login/emby") => Ok(login_response(
            source.emby_connect_login(body(request).await?),
        )),
        (&Method::POST, "/api/sources/login/jellyfin") => {
            #[derive(Deserialize)]
            struct Input {
                server_url: String,
                #[serde(default)]
                trust_invalid_cert: bool,
            }
            let input: Input = body(request).await?;
            Ok(login_response(source.jellyfin_quick_connect(
                input.server_url,
                input.trust_invalid_cert,
            )))
        }
        (&Method::POST, "/api/sources/login/nextcloud") => {
            let input: Files = body(request).await?;
            Ok(login_response(
                source.nextcloud_login(input.settings, input.credentials),
            ))
        }
        (&Method::POST, "/api/sources/smb/shares") => {
            let input: Files = body(request).await?;
            let shares = completion(source.smb_shares(input.settings, input.credentials)).await?;
            Ok(json_response(StatusCode::OK, json!({"shares":shares})))
        }
        (&Method::POST, "/api/sources/plex/profiles") => {
            #[derive(Deserialize)]
            struct Input {
                login: sources::PlexLogin,
            }
            let (login, profiles) =
                completion(source.plex_profiles(body::<Input>(request).await?.login)).await?;
            Ok(json_response(
                StatusCode::OK,
                json!({"login":login,"profiles":profiles}),
            ))
        }
        (&Method::GET, "/api/sources/plex/logins") => {
            let logins = completion(source.plex_saved_logins()).await?;
            Ok(json_response(
                StatusCode::OK,
                json!({"logins":logins.into_iter().map(|(name, login)| json!({"name":name,"login":login})).collect::<Vec<_>>()}),
            ))
        }
        (&Method::POST, "/api/sources/plex/servers") => {
            #[derive(Deserialize)]
            struct Input {
                login: sources::PlexLogin,
                profile: sources::PlexProfile,
                pin: Option<String>,
                #[serde(default)]
                lan: Vec<sources::DiscoveredServer>,
            }
            let input: Input = body(request).await?;
            let (login, servers) =
                completion(source.plex_servers(input.login, input.profile, input.pin, input.lan))
                    .await?;
            Ok(json_response(
                StatusCode::OK,
                json!({"login":login,"servers":servers}),
            ))
        }
        _ => Err((StatusCode::NOT_FOUND, "Route not found".into())),
    }
}

#[derive(Deserialize)]
struct Files {
    settings: sources::FileSourceSettings,
    credentials: sources::FileCredentials,
}

fn login_response<T: serde::Serialize + Send + 'static>(
    receiver: async_channel::Receiver<Result<T, String>>,
) -> Response<Body> {
    let stream = futures_util::stream::unfold(receiver, |receiver| async move {
        let event = receiver.recv().await.ok()?;
        let value = match event {
            Ok(event) => serde_json::to_value(event)
                .unwrap_or_else(|error| json!({"error":error.to_string()})),
            Err(error) => json!({"error":error}),
        };
        Some((
            Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                "event: login\ndata: {value}\n\n"
            )))),
            receiver,
        ))
    });
    event_response(BodyExt::boxed(StreamBody::new(stream)))
}
