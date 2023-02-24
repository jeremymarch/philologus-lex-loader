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

static OUTPUT: &str = "output.txt";

#[derive(Clone)]
struct Lexicon {
    dir_name: String,
    file_name: String,
    repo_url: String,
    start_rng: u32,
    end_rng: u32,
}

struct Processor {
    lexica: Vec<Lexicon>,
    index: Index,
    index_writer: IndexWriter,
    db: AnyConnection,
}

impl Processor {
    async fn db_insert_word(&mut self, item_count: i32, lemma: &str, def: &str) {
        //println!("{} {}", item_count, lemma);
        let query = r#"INSERT INTO ZGREEK (seq, word, sortword, def) VALUES ($1, $2, $3, $4);"#;
        let _ = sqlx::query(query)
            .bind(item_count)
            .bind(lemma)
            .bind(lemma)
            .bind(def)
            .execute(&mut self.db)
            .await
            .unwrap();
    }

    fn tantivy_insert_word(
        &mut self,
        _item_count: i32,
        lemma: &str,
        item_text_no_tags: &str,
        title: &Field,
        body: &Field,
    ) {
        //println!("{} {}", item_count, lemma);
        let mut doc = Document::default();
        doc.add_text(*title, lemma);
        doc.add_text(*body, item_text_no_tags);
        self.index_writer.add_document(doc).unwrap();
    }

    async fn read_xml(&mut self, file: &str, item_count: &mut i32) {
        //println!("file: {}", file);
        let mut reader = Reader::from_file(file).unwrap();
        reader.trim_text(false); //false to preserve whitespace

        let mut buf = Vec::new();

        let mut item_text = String::from("");
        let mut item_text_no_tags = String::from("");
        let mut head = String::from("");
        let mut orth = String::from("");

        let mut sense_count = 0;
        let mut in_orth_tag = false;
        let mut in_head_tag = false;
        let mut in_text_tag = false;

        // let mut file = OpenOptions::new()
        //     .append(true)
        //     .create(true)
        //     .open(OUTPUT)
        //     .unwrap();

        let title = self.index.schema().get_field("title").unwrap();
        let body = self.index.schema().get_field("body").unwrap();

        loop {
            match reader.read_event_into(&mut buf) {
                Err(e) => panic!("Error at position {}: {:?}", reader.buffer_position(), e),
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
                        item_text.push_str(r#"<span class="orth">"#);
                    }
                    b"div1" => {
                        head.clear();
                        orth.clear();
                        item_text.clear();
                        sense_count = 0;
                        item_text.push_str(r#"<div id=""#);
                        let mut found_id = false;
                        for a in e.attributes() {
                            if a.as_ref().unwrap().key == QName(b"id") {
                                found_id = true;
                                item_text.push_str(std::str::from_utf8(&a.unwrap().value).unwrap());
                                break;
                            }
                        }
                        item_text.push_str(r#"" class="body">"#);
                        // checking that we found an id prevents treating container <div1> as a word div in lsj
                        if !found_id {
                            item_text.clear();
                        }
                    }
                    b"div2" => {
                        head.clear();
                        orth.clear();
                        item_text.clear();
                        sense_count = 0;
                        item_text.push_str(r#"<div id=""#);
                        let mut found_id = false;
                        for a in e.attributes() {
                            if a.as_ref().unwrap().key == QName(b"id") {
                                found_id = true;
                                item_text.push_str(std::str::from_utf8(&a.unwrap().value).unwrap());
                                break;
                            }
                        }
                        item_text.push_str(r#"" class="body">"#);
                        if !found_id {
                            item_text.clear();
                        }
                    }
                    b"sense" => {
                        if sense_count == 0 {
                            item_text.push_str(r#"<br/><br/><div class="l"#);
                        } else {
                            item_text.push_str(r#"<br/><div class="l"#);
                        }
                        let mut label = String::from("");
                        for a in e.attributes() {
                            if a.as_ref().unwrap().key == QName(b"level") {
                                item_text.push_str(std::str::from_utf8(&a.unwrap().value).unwrap());
                            } else if a.as_ref().unwrap().key == QName(b"n") {
                                label.push_str(std::str::from_utf8(&a.unwrap().value).unwrap());
                            }
                        }
                        item_text.push_str(r#"">"#);
                        if !label.is_empty() {
                            item_text.push_str(
                                format!(r#"<span class="label">{}.</span>"#, label).as_str(),
                            );
                        }
                        sense_count += 1;
                    }
                    b"author" => {
                        item_text.push_str(r#"<span class="au">"#);
                    }
                    b"quote" => {
                        item_text.push_str(r#"<span class="qu">"#);
                    }
                    b"foreign" => {
                        item_text.push_str(r#"<span class="fo">"#);
                    }
                    b"i" => {
                        item_text.push_str(r#"<span class="tr">"#);
                    }
                    b"title" => {
                        item_text.push_str(r#"<span class="ti">"#);
                    }
                    b"bibl" => {
                        item_text.push_str(r#"<a class="bi" biblink=""#);
                        for a in e.attributes() {
                            if a.as_ref().unwrap().key == QName(b"n") {
                                item_text.push_str(std::str::from_utf8(&a.unwrap().value).unwrap());
                                break;
                            }
                        }
                        item_text.push_str(r#"">"#);
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
                            item_text.push_str("</span>");
                        }
                        b"div1" => {
                            item_text.push_str("</div>");
                            //println!("item: {}", item_text);
                            if in_text_tag && item_text.len() > 6 {
                                //writeln!(file, "{} {}", item_count, item_text).unwrap();
                                //self.db_insert_word(*item_count, &head, &item_text).await;
                            }
                            head.clear();
                            orth.clear();
                            item_text.clear();
                            item_text_no_tags.clear();
                        }
                        b"div2" => {
                            item_text.push_str("</div>");
                            //println!("item: {}", item_text);
                            if in_text_tag && item_text.len() > 6 {
                                *item_count += 1;
                                //writeln!(file, "{} {}", item_count, item_text).unwrap();
                                self.db_insert_word(*item_count, &head, &item_text).await;

                                self.tantivy_insert_word(
                                    *item_count,
                                    &head,
                                    &item_text_no_tags,
                                    &title,
                                    &body,
                                );
                            }
                            head.clear();
                            orth.clear();
                            item_text.clear();
                            item_text_no_tags.clear();
                        }
                        b"sense" => {
                            item_text.push_str("</div>");
                        }
                        b"author" => {
                            item_text.push_str("</span>");
                        }
                        b"quote" => {
                            item_text.push_str("</span>");
                        }
                        b"foreign" => {
                            item_text.push_str("</span>");
                        }
                        b"i" => {
                            item_text.push_str("</span>");
                        }
                        b"title" => {
                            item_text.push_str("</span>");
                        }
                        b"bibl" => {
                            item_text.push_str("</a>");
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
                        head.push_str(&e.unescape().unwrap());
                    }
                    if in_orth_tag {
                        orth.push_str(&e.unescape().unwrap());
                    }
                    item_text.push_str(&e.unescape().unwrap());
                    item_text_no_tags.push_str(&e.unescape().unwrap());
                }
            }
            buf.clear();
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if Path::new(OUTPUT).is_file() {
        fs::remove_file(OUTPUT).expect("File delete failed");
    }

    let mut conn = AnyConnection::connect("sqlite://db.sqlite?mode=rwc").await?;

    let query = "CREATE TABLE IF NOT EXISTS ZGREEK(seq INTEGER PRIMARY KEY, word TEXT, sortword TEXT, def TEXT);";
    let _res = sqlx::query(query).execute(&mut conn).await;

    let query = "DELETE FROM ZGREEK;";
    let _res = sqlx::query(query).execute(&mut conn).await;

    let lsj = Lexicon {
        dir_name: "LSJLogeion/".to_string(),
        file_name: "greatscott".to_string(),
        repo_url: "https://github.com/helmadik/LSJLogeion.git".to_string(),
        start_rng: 2,
        end_rng: 86,
    };

    let index_path = TempDir::new()?; //"tantivy-data"; //
    let mut schema_builder = Schema::builder();
    schema_builder.add_text_field("title", TEXT | STORED);
    schema_builder.add_text_field("body", TEXT | STORED);
    let schema = schema_builder.build();
    let index = Index::create_in_dir(&index_path, schema.clone())?;
    let index_writer: IndexWriter = index.writer(50_000_000)?;

    let mut p = Processor {
        lexica: vec![lsj],
        index,
        index_writer,
        db: conn,
    };

    let mut item_count: i32 = 0;

    for lex in p.lexica.clone() {
        if !Path::new(&lex.dir_name).exists() {
            println!("Cloning {}...", &lex.repo_url);

            let _repo = match Repository::clone(&lex.repo_url, &lex.dir_name) {
                Ok(repo) => repo,
                Err(e) => panic!("failed to clone: {}", e),
            };
        }

        for i in lex.start_rng..=lex.end_rng {
            let path = format!("{}{}{:02}.xml", &lex.dir_name, &lex.file_name, i);
            p.read_xml(&path, &mut item_count).await;
        }
        println!("items: {}", item_count);
    }

    p.index_writer.commit().unwrap();

    let title = p.index.schema().get_field("title").unwrap();
    let body = p.index.schema().get_field("body").unwrap();

    let reader = p
        .index
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommit)
        .try_into()?;

    let searcher = reader.searcher();

    let query_parser = QueryParser::for_index(&p.index, vec![title, body]);
    let query = query_parser.parse_query("ἐπόχους")?;

    let top_docs = searcher.search(&query, &TopDocs::with_limit(10))?;

    for (_score, doc_address) in top_docs {
        let retrieved_doc = searcher.doc(doc_address)?;
        println!("{}", schema.to_json(&retrieved_doc));
    }

    Ok(())
}
