use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, IndexWriter, ReloadPolicy};
use tempfile::TempDir;

use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::reader::Reader;

use git2::Repository;
use std::fs;
use std::path::Path;

// use quick_xml::events::BytesStart;
// use std::io::Cursor;
// use quick_xml::Writer;
// use quick_xml::events::BytesEnd;

//use std::fs::OpenOptions;
//use std::io::prelude::*;

use sqlx::AnyConnection;
use sqlx::Connection;

use polytonic_greek::hgk_strip_diacritics;

static OUTPUT: &str = "output.txt";

#[derive(Clone)]
struct Lexicon<'a> {
    dir_name: &'a str,
    file_name: &'a str,
    repo_url: &'a str,
    start_rng: u32,
    end_rng: u32,
    name: &'a str,
}

struct LexEntryCollector {
    item_text: String,
    item_text_no_tags: String,
    head: String,
    orth: String,
    sense_count: u32,
}

impl LexEntryCollector {
    fn new() -> Self {
        Self {
            item_text: String::from(""),
            item_text_no_tags: String::from(""),
            head: String::from(""),
            orth: String::from(""),
            sense_count: 0,
        }
    }

    fn clear(&mut self) {
        self.item_text.clear();
        self.item_text_no_tags.clear();
        self.head.clear();
        self.orth.clear();
        self.sense_count = 0;
    }
}

fn sanitize_sort_key(str: &str) -> String {
    match str {
        "σάν" => return "πωω".to_string(),
        "Ϟ ϟ" => return "πωωω".to_string(),
        _ => (),
    }
    let mut s = str.to_lowercase();
    s = hgk_strip_diacritics(&s, 0xFFFFFFFF);
    s = s.replace('\u{1fbd}', "");
    s = s.replace('ʼ', "");
    s = s.replace('ϝ', "εωωω");
    s = s.replace("'st", "st2");
    s
}

struct Processor<'a> {
    lexica: Vec<Lexicon<'a>>,
    index_writer: IndexWriter,
    db: AnyConnection,
}

impl Processor<'_> {
    async fn db_insert_word<'a, 'b>(
        tx: &'a mut sqlx::Transaction<'b, sqlx::Any>,
        item_count: i32,
        lexicon_name: &str,
        lemma: &str,
        def: &str,
    ) -> Result<(), sqlx::Error> {
        //println!("{} {}", item_count, lemma);
        let query =
            r#"INSERT INTO words (seq, lexicon, word, sortword, def) VALUES ($1, $2, $3, $4, $5);"#;
        let _ = sqlx::query(query)
            .bind(item_count)
            .bind(lexicon_name)
            .bind(lemma)
            .bind(sanitize_sort_key(lemma).as_str())
            .bind(def)
            .execute(&mut *tx)
            .await?;
        Ok(())
    }

    fn tantivy_insert_word(
        index_writer: &IndexWriter,
        item_count: i32,
        lemma: &str,
        lexicon_name: &str,
        item_text_no_tags: &str,
    ) {
        let word_id_field = index_writer.index().schema().get_field("word_id").unwrap();
        let lemma_field = index_writer.index().schema().get_field("lemma").unwrap();
        let lexicon_field = index_writer.index().schema().get_field("lexicon").unwrap();
        let def_field = index_writer
            .index()
            .schema()
            .get_field("definition")
            .unwrap();

        //println!("{} {}", item_count, lemma);
        let mut doc = Document::default();
        doc.add_u64(word_id_field, item_count.try_into().unwrap());
        doc.add_text(lemma_field, lemma);
        doc.add_text(lexicon_field, lexicon_name);
        doc.add_text(def_field, item_text_no_tags);
        index_writer.add_document(doc).unwrap();
    }

    async fn read_xml(
        &mut self,
        file: &str,
        lexicon_name: &str,
        item_count: &mut i32,
    ) -> Result<(), sqlx::Error> {
        //println!("file: {}", file);
        let mut reader = Reader::from_file(file).unwrap();
        reader.trim_text(false); //false to preserve whitespace

        let mut buf = Vec::new();

        let mut entry = LexEntryCollector::new();

        let mut in_orth_tag = false;
        let mut in_head_tag = false;
        let mut in_text_tag = false;

        // let mut file = OpenOptions::new()
        //     .append(true)
        //     .create(true)
        //     .open(OUTPUT)
        //     .unwrap();

        let mut tx = self.db.begin().await?;

        loop {
            match reader.read_event_into(&mut buf) {
                Err(e) => panic!(
                    "XML parsing error at position {}: {:?}",
                    reader.buffer_position(),
                    e
                ),
                Ok(Event::Eof) => break,
                Ok(Event::Comment(_e)) => {}
                Ok(Event::CData(_e)) => {}
                Ok(Event::Decl(_e)) => {}
                Ok(Event::PI(_e)) => {}
                Ok(Event::DocType(_e)) => {}

                Ok(Event::Start(e)) => match e.name().as_ref() {
                    b"text" => {
                        in_text_tag = true;
                    }
                    b"head" => {
                        in_head_tag = true;
                    }
                    b"orth" => {
                        in_orth_tag = true;
                        entry.item_text.push_str(r#"<span class="orth">"#);
                    }
                    b"div1" | b"div2" => {
                        entry.clear();

                        entry.item_text.push_str(r#"<div id=""#);
                        let mut found_id = false;
                        for a in e.attributes() {
                            if a.as_ref().unwrap().key == QName(b"id") {
                                found_id = true;
                                entry
                                    .item_text
                                    .push_str(std::str::from_utf8(&a.unwrap().value).unwrap());
                                break;
                            }
                        }
                        entry.item_text.push_str(r#"" class="body">"#);
                        // checking that we found an id prevents treating container <div1> as a word div in lsj
                        if !found_id {
                            entry.clear();
                        }
                    }
                    b"sense" => {
                        if entry.sense_count == 0 {
                            entry.item_text.push_str(r#"<br/><br/><div class="l"#);
                        } else {
                            entry.item_text.push_str(r#"<br/><div class="l"#);
                        }
                        let mut label = String::from("");
                        for a in e.attributes() {
                            if a.as_ref().unwrap().key == QName(b"level") {
                                entry
                                    .item_text
                                    .push_str(std::str::from_utf8(&a.unwrap().value).unwrap());
                            } else if a.as_ref().unwrap().key == QName(b"n") {
                                label.push_str(std::str::from_utf8(&a.unwrap().value).unwrap());
                            }
                        }
                        entry.item_text.push_str(r#"">"#);
                        if !label.is_empty() {
                            entry.item_text.push_str(
                                format!(r#"<span class="label">{}.</span>"#, label).as_str(),
                            );
                        }
                        entry.sense_count += 1;
                    }
                    b"author" => {
                        entry.item_text.push_str(r#"<span class="au">"#);
                    }
                    b"quote" => {
                        entry.item_text.push_str(r#"<span class="qu">"#);
                    }
                    b"foreign" => {
                        entry.item_text.push_str(r#"<span class="fo">"#);
                    }
                    b"i" => {
                        entry.item_text.push_str(r#"<span class="tr">"#);
                    }
                    b"title" => {
                        entry.item_text.push_str(r#"<span class="ti">"#);
                    }
                    b"bibl" => {
                        entry.item_text.push_str(r#"<a class="bi" biblink=""#);
                        for a in e.attributes() {
                            if a.as_ref().unwrap().key == QName(b"n") {
                                entry
                                    .item_text
                                    .push_str(std::str::from_utf8(&a.unwrap().value).unwrap());
                                break;
                            }
                        }
                        entry.item_text.push_str(r#"">"#);
                    }
                    _ => (),
                },
                Ok(Event::End(e)) => {
                    match e.name().as_ref() {
                        b"text" => {
                            in_text_tag = false;
                        }
                        b"head" => {
                            in_head_tag = false;
                        }
                        b"orth" => {
                            in_orth_tag = false;
                            entry.item_text.push_str("</span>");
                        }
                        b"div1" => {
                            entry.item_text.push_str("</div>");

                            if in_text_tag && entry.item_text.trim().len() > 6 {
                                *item_count += 1;
                                //writeln!(file, "{} {}", item_count, item_text).unwrap();

                                //this fixes issue in borrow checker with multiple mutable refs to self
                                let index_writer = &self.index_writer;
                                Processor::tantivy_insert_word(
                                    index_writer,
                                    *item_count,
                                    &entry.head,
                                    lexicon_name,
                                    entry.item_text_no_tags.trim(),
                                );

                                let _ = Processor::db_insert_word(
                                    &mut tx,
                                    *item_count,
                                    lexicon_name,
                                    &entry.head,
                                    &entry.item_text,
                                )
                                .await;

                                //println!("item: {}", item_text);
                            }
                            entry.clear();
                        }
                        b"div2" => {
                            entry.item_text.push_str("</div>");
                            //println!("item: {}", item_text);
                            if in_text_tag && entry.item_text.trim().len() > 6 {
                                *item_count += 1;
                                //writeln!(file, "{} {}", item_count, item_text).unwrap();

                                //this fixes issue in borrow checker with multiple mutable refs to self
                                let index_writer = &self.index_writer;
                                Processor::tantivy_insert_word(
                                    index_writer,
                                    *item_count,
                                    &entry.head,
                                    lexicon_name,
                                    entry.item_text_no_tags.trim(),
                                );

                                Processor::db_insert_word(
                                    &mut tx,
                                    *item_count,
                                    lexicon_name,
                                    &entry.head,
                                    &entry.item_text,
                                )
                                .await
                                .unwrap();
                            }
                            entry.clear();
                        }
                        b"sense" => {
                            entry.item_text.push_str("</div>");
                        }
                        b"author" => {
                            entry.item_text.push_str("</span>");
                        }
                        b"quote" => {
                            entry.item_text.push_str("</span>");
                        }
                        b"foreign" => {
                            entry.item_text.push_str("</span>");
                        }
                        b"i" => {
                            entry.item_text.push_str("</span>");
                        }
                        b"title" => {
                            entry.item_text.push_str("</span>");
                        }
                        b"bibl" => {
                            entry.item_text.push_str("</a>");
                        }
                        _ => (),
                    }
                }
                Ok(Event::Empty(_e)) => {}
                Ok(Event::Text(e)) => {
                    //txt.push(e.unescape().unwrap().into_owned())
                    // if in_head_tag {

                    // }
                    if in_head_tag {
                        entry.head.push_str(&e.unescape().unwrap());
                    }
                    if in_orth_tag {
                        entry.orth.push_str(&e.unescape().unwrap());
                    }
                    entry.item_text.push_str(&e.unescape().unwrap());
                    entry.item_text_no_tags.push_str(&e.unescape().unwrap());
                }
            }
            buf.clear();
        }
        tx.commit().await?;
        Ok(())
    }

    async fn start(&mut self) -> Result<(), sqlx::Error> {
        let query = "CREATE TABLE IF NOT EXISTS words (seq INTEGER PRIMARY KEY, lexicon TEXT, word TEXT, sortword TEXT, def TEXT); \
        CREATE INDEX IF NOT EXISTS lexicon_idx ON words (lexicon);";
        let _res = sqlx::query(query).execute(&mut self.db).await;

        let query = "DELETE FROM words;";
        let _res = sqlx::query(query).execute(&mut self.db).await;

        let mut item_count: i32 = 0;

        for lex in self.lexica.clone() {
            if !Path::new(&lex.dir_name).exists() {
                println!("Cloning {}...", &lex.repo_url);

                let _repo = match Repository::clone(lex.repo_url, lex.dir_name) {
                    Ok(repo) => repo,
                    Err(e) => panic!("failed to clone: {}", e),
                };
            }

            for i in lex.start_rng..=lex.end_rng {
                let path = format!("{}{}{:02}.xml", &lex.dir_name, &lex.file_name, i);
                //println!("path: {}", path);
                if lex.file_name == "latindico" && i == 10 {
                    //there is no file for words starting with "j"
                    continue;
                }
                let _ = self.read_xml(&path, lex.name, &mut item_count).await;
            }
            println!("items: {}", item_count);
        }

        self.index_writer.commit().unwrap();
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if Path::new(OUTPUT).is_file() {
        fs::remove_file(OUTPUT).expect("File delete failed");
    }

    let lsj = Lexicon {
        dir_name: "LSJLogeion/",
        file_name: "greatscott",
        repo_url: "https://github.com/helmadik/LSJLogeion.git",
        start_rng: 2,
        end_rng: 86,
        name: "lsj",
    };
    let ls = Lexicon {
        dir_name: "LewisShortLogeion/",
        file_name: "latindico",
        repo_url: "https://github.com/helmadik/LewisShortLogeion.git",
        start_rng: 1,
        end_rng: 25,
        name: "lewisshort",
    };
    let slater = Lexicon {
        dir_name: "SlaterPindar/",
        file_name: "pindar_dico",
        repo_url: "https://github.com/helmadik/SlaterPindar.git",
        start_rng: 1,
        end_rng: 24,
        name: "slater",
    };

    let index_path = TempDir::new()?; // "tantivy-data";
    let mut schema_builder = Schema::builder();

    let num_options = NumericOptions::default().set_stored().set_indexed();
    schema_builder.add_u64_field("word_id", num_options);
    schema_builder.add_text_field("lemma", STORED); // lemma is also in definition, so no need to index it separately
    schema_builder.add_text_field("lexicon", TEXT | STORED);
    schema_builder.add_text_field("definition", TEXT);
    let schema = schema_builder.build();

    let index = Index::create_in_dir(&index_path, schema.clone())?;
    //let index = Index::create_in_ram(schema.clone());
    let index_writer: IndexWriter = index.writer(50_000_000)?;

    let conn = AnyConnection::connect("sqlite://db.sqlite?mode=rwc").await?;

    let mut processor = Processor {
        lexica: vec![lsj, ls, slater],
        index_writer,
        db: conn,
    };

    processor.start().await.unwrap();

    //let word_id_field = index.schema().get_field("word_id").unwrap();
    //let lemma_field = index.schema().get_field("lemma").unwrap();
    let lexicon_field = index.schema().get_field("lexicon").unwrap();
    let definition_field = index.schema().get_field("definition").unwrap();

    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommit)
        .try_into()?;

    let searcher = reader.searcher();
    let query_parser = QueryParser::for_index(
        &index,
        //this vector contains default fields used if field is not specified in query
        vec![lexicon_field, definition_field],
    );

    match query_parser.parse_query("carry AND (lexicon:slater OR lexicon:lewisshort)") {
        Ok(query) => {
            let top_docs = searcher.search(&query, &TopDocs::with_limit(100))?;
            for (_score, doc_address) in top_docs {
                let retrieved_doc = searcher.doc(doc_address)?;
                println!("{}", schema.to_json(&retrieved_doc));
            }
        }
        Err(q) => {
            println!("Query parsing error: {:?}", q);
        }
    }

    Ok(())
}
