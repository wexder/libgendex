# Search and ranking

Search runs BM25 over title (×3), author (×2), series and publisher with ASCII folding, so
`exupery` finds *Saint-Exupéry*; ISBNs match exactly. All terms are required first, then one-typo
fuzzy matching, then any term. Results are then ordered with file heuristics: format preference
(EPUB > AZW3 > MOBI > FB2 > … > PDF > DJVU), file-size sanity, metadata completeness and a
study-guide/summary keyword penalty.

**AI re-ranking is an optional component** (`ranking.provider`, default `none`). When enabled it
re-orders the top `rerank_top` hits using OpenJev-style typed questions, answered from model
probabilities rather than generated text:

| `provider` | What runs | Cost |
|---|---|---|
| `none` | nothing extra | – |
| `api` | each hit is sent to a remote [OpenJev](https://github.com/razorback16/openjev) (or TypeSafe Jev) `/v1/systemone` server: relevance (score), e-reader suitability (score), genuine edition (yes/no) | network only |
| `local` | in-process [Qwen3-1.7B](https://huggingface.co/Qwen/Qwen3-1.7B) (Q4_K_M GGUF via llama.cpp) on the CPU; needs a build with the `local` feature | ~1.1 GB model download, ~0.9 GB RAM, 2–4 s per new query on a 6-core desktop CPU |

`local` scores the top 8 distinct works (title + author, so all formats of a book share one
answer) with two yes/no questions: *is this exactly the book being searched for?* and *is this a
study guide, summary or companion rather than the book itself?* The query and each work are
evaluated once and the questions reuse that KV cache. Answers are log-odds, calibrated within
the search, and cached per (query, work). E-reader fit always comes from the file heuristic.

Whatever the provider, a failure or timeout falls back to the heuristic, and the UI first shows
instant results (`?ai=false`) and swaps in the AI-ranked list when it arrives.

Building with the local model:

```sh
cargo build --release --features local                        # needs cmake + a C++ toolchain
docker build --build-arg FEATURES=local -t libgendex:local .     # or uncomment the arg in docker-compose.yml
```
