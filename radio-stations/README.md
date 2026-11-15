# Radio stations

The stations Rufin suggests on its Radio page. Each station is one JSON file here; the file
name is the station's id. Rufin reads every `.json` file in this folder when it is built.

```json
{
  "name": "LISTEN.moe",
  "description": "Japanese and Korean pop around the clock, played by request.",
  "homepage": "https://listen.moe",
  "genres": ["Anime", "J-pop", "K-pop"],
  "logo": "https://example.org/logo.png",
  "channels": [
    { "name": "J-pop", "stream": "https://listen.moe/fallback" },
    { "name": "K-pop", "stream": "https://listen.moe/kpop/fallback" }
  ]
}
```

- `name` and at least one channel are required. A station with one stream has one channel,
  named like the station.
- `stream` is the address of the audio itself, over https, not a `.pls` or `.m3u` playlist.
  Prefer a stream that announces its songs: MP3 and AAC streams usually do.
- `description` is one short sentence, the station's own where it has one.
- `logo` is optional: the address of a square picture the station publishes.

To add a station, add its file and open a pull request.
