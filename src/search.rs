use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term,
    collector::{Count, TopDocs},
    doc,
    query::{BooleanQuery, Occur, Query, QueryParser, TermQuery},
    schema::{
        FAST, Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
        Value,
    },
    tokenizer::{AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer},
};
use utoipa::ToSchema;

const TOKENIZER: &str = "folded";

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct Book {
    pub md5: String,
    pub title: String,
    pub author: String,
    pub series: String,
    pub publisher: String,
    pub year: String,
    pub language: String,
    pub extension: String,
    pub filesize: u64,
    pub pages: u64,
    pub isbn: Vec<String>,
    pub source: String,
}

#[derive(Clone, Copy)]
struct Fields {
    md5: Field,
    title: Field,
    author: Field,
    series: Field,
    publisher: Field,
    year: Field,
    language: Field,
    extension: Field,
    filesize: Field,
    pages: Field,
    isbn: Field,
    source: Field,
}

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    f: Fields,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let text = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored();
    let f = Fields {
        md5: b.add_text_field("md5", STRING | STORED),
        title: b.add_text_field("title", text.clone()),
        author: b.add_text_field("author", text.clone()),
        series: b.add_text_field("series", text.clone()),
        publisher: b.add_text_field("publisher", text),
        year: b.add_text_field("year", STORED),
        language: b.add_text_field("language", STRING | STORED),
        extension: b.add_text_field("extension", STRING | STORED),
        filesize: b.add_u64_field("filesize", STORED | FAST),
        pages: b.add_u64_field("pages", STORED),
        isbn: b.add_text_field("isbn", STRING | STORED),
        source: b.add_text_field("source", STRING | STORED),
    };
    (b.build(), f)
}

impl SearchIndex {
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let (schema, f) = schema();
        let index = Index::open_or_create(tantivy::directory::MmapDirectory::open(dir)?, schema)?;
        index.tokenizers().register(
            TOKENIZER,
            TextAnalyzer::builder(SimpleTokenizer::default())
                .filter(RemoveLongFilter::limit(40))
                .filter(LowerCaser)
                .filter(AsciiFoldingFilter)
                .build(),
        );
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        Ok(Self { index, reader, f })
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    pub fn writer(&self, memory_mb: usize) -> Result<BookWriter> {
        let writer = self
            .index
            .writer_with_num_threads(1, memory_mb.max(15) * 1_000_000)?;
        Ok(BookWriter { writer, f: self.f })
    }

    pub fn reload(&self) -> Result<()> {
        Ok(self.reader.reload()?)
    }

    pub fn search(
        &self,
        query: &str,
        extension: Option<&str>,
        language: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(f32, Book)>> {
        let f = self.f;
        let searcher = self.reader.searcher();
        // Require all terms first, then tolerate typos, then accept any matching term.
        let attempts = [(true, false), (true, true), (false, false)];
        for (i, (conjunction, fuzzy)) in attempts.into_iter().enumerate() {
            let mut parser = QueryParser::for_index(
                &self.index,
                vec![f.title, f.author, f.series, f.publisher, f.isbn],
            );
            parser.set_field_boost(f.title, 3.0);
            parser.set_field_boost(f.author, 2.0);
            parser.set_field_boost(f.series, 1.2);
            parser.set_field_boost(f.publisher, 0.5);
            if conjunction {
                parser.set_conjunction_by_default();
            }
            if fuzzy {
                for field in [f.title, f.author, f.series] {
                    parser.set_field_fuzzy(field, false, 1, true);
                }
            }
            let (text_query, _) = parser.parse_query_lenient(query);
            let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, text_query)];
            // Filters accept comma-separated alternatives, e.g. `ger,deu`.
            for (field, value) in [(f.extension, extension), (f.language, language)] {
                let alternatives: Vec<(Occur, Box<dyn Query>)> = value
                    .unwrap_or_default()
                    .split(',')
                    .map(|v| v.trim().to_lowercase())
                    .filter(|v| !v.is_empty())
                    .map(|v| {
                        let q: Box<dyn Query> = Box::new(TermQuery::new(
                            Term::from_field_text(field, &v),
                            IndexRecordOption::Basic,
                        ));
                        (Occur::Should, q)
                    })
                    .collect();
                if !alternatives.is_empty() {
                    clauses.push((Occur::Must, Box::new(BooleanQuery::new(alternatives))));
                }
            }
            let top = searcher.search(
                &BooleanQuery::new(clauses),
                &TopDocs::with_limit(limit).order_by_score(),
            )?;
            if !top.is_empty() || i == attempts.len() - 1 {
                return top
                    .into_iter()
                    .map(|(score, addr)| Ok((score, self.to_book(&searcher.doc(addr)?))))
                    .collect();
            }
        }
        Ok(Vec::new())
    }

    pub fn contains(&self, md5: &str) -> bool {
        let q = TermQuery::new(
            Term::from_field_text(self.f.md5, &md5.to_lowercase()),
            IndexRecordOption::Basic,
        );
        self.reader
            .searcher()
            .search(&q, &Count)
            .is_ok_and(|n| n > 0)
    }

    pub fn get(&self, md5: &str) -> Result<Option<Book>> {
        let searcher = self.reader.searcher();
        let q = TermQuery::new(
            Term::from_field_text(self.f.md5, &md5.to_lowercase()),
            IndexRecordOption::Basic,
        );
        let top = searcher.search(&q, &TopDocs::with_limit(1).order_by_score())?;
        match top.first() {
            Some((_, addr)) => Ok(Some(self.to_book(&searcher.doc(*addr)?))),
            None => Ok(None),
        }
    }

    fn to_book(&self, d: &TantivyDocument) -> Book {
        let f = self.f;
        let s = |field| {
            d.get_first(field)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let n = |field| {
            d.get_first(field)
                .and_then(|v| v.as_u64())
                .unwrap_or_default()
        };
        Book {
            md5: s(f.md5),
            title: s(f.title),
            author: s(f.author),
            series: s(f.series),
            publisher: s(f.publisher),
            year: s(f.year),
            language: s(f.language),
            extension: s(f.extension),
            filesize: n(f.filesize),
            pages: n(f.pages),
            isbn: d
                .get_all(f.isbn)
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            source: s(f.source),
        }
    }
}

pub struct BookWriter {
    writer: IndexWriter,
    f: Fields,
}

impl BookWriter {
    pub fn delete_source(&mut self, source: &str) {
        self.writer
            .delete_term(Term::from_field_text(self.f.source, source));
    }

    pub fn delete_md5(&mut self, md5: &str) {
        self.writer
            .delete_term(Term::from_field_text(self.f.md5, &md5.to_lowercase()));
    }

    pub fn add(&mut self, b: &Book) -> Result<()> {
        let f = self.f;
        let mut d = doc!(
            f.md5 => b.md5.to_lowercase(),
            f.title => b.title.as_str(),
            f.author => b.author.as_str(),
            f.series => b.series.as_str(),
            f.publisher => b.publisher.as_str(),
            f.year => b.year.as_str(),
            f.language => b.language.to_lowercase(),
            f.extension => b.extension.to_lowercase(),
            f.filesize => b.filesize,
            f.pages => b.pages,
            f.source => b.source.as_str(),
        );
        for isbn in &b.isbn {
            d.add_text(f.isbn, isbn);
        }
        self.writer.add_document(d)?;
        Ok(())
    }

    pub fn commit(mut self) -> Result<()> {
        self.writer.commit()?;
        self.writer.wait_merging_threads()?;
        Ok(())
    }

    pub fn rollback(mut self) -> Result<()> {
        self.writer.rollback()?;
        Ok(())
    }
}
