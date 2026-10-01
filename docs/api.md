# Downloads and API

## Downloads

- **Download** streams the file through the server to the browser with a proper file name.
- **Save to library** downloads on the server to `<library>/<First Author>/<Title> (<year>).<ext>`
  (written to `.part` and renamed on completion). Progress, completion, and retry states appear on the search result’s save button.

File links are resolved through `download.resolvers`: by default libgen.li-style mirrors, whose
`ads.php?md5=` page links a `get.php` URL.

## API

OpenAPI spec: `GET /api/openapi.json` (also committed as [`openapi.json`](../openapi.json)).
The frontend client in `web/src/api` is generated from it with `@hey-api/openapi-ts`.

| | |
|---|---|
| `GET /api/search?q=&ext=&lang=&limit=&ai=` | ranked results (`ai=false`: skip the AI rerank) |
| `GET /api/books/{md5}` | metadata |
| `GET /api/books/{md5}/file` | stream the file to the client |
| `POST /api/books/{md5}/save` | queue a library download |
| `GET /api/downloads` | library download jobs |
| `GET /api/index/status`, `POST /api/index/refresh` | indexer state / trigger a refresh |
| `GET /api/health` | liveness + book count |
