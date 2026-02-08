use std::collections::HashMap;

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, IndexWriter, ReloadPolicy};
use tantivy::tokenizer::*;
// use tempfile::TempDir;

use quick_xml::events::Event;
use quick_xml::name::QName;
use quick_xml::reader::Reader;

use git2::Repository;

use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;

// use quick_xml::events::BytesStart;
// use std::io::Cursor;
// use quick_xml::Writer;
// use quick_xml::events::BytesEnd;

use sqlx::AnyConnection;
use sqlx::Connection;
use sqlx::any::install_default_drivers;

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
    branch: &'a str,
    remote: &'a str,
    pull: bool,
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
        "σάν" => return "πωω".to_string(),                // after pi
        "\u{03DE} \u{03DF}" => return "πωωω".to_string(), // koppa and lower case koppa are after san
        _ => (),
    }
    let mut s = str.to_lowercase();
    s = hgk_strip_diacritics(&s, 0xFFFFFFFF); // strip all diacritics
    s = s.replace('\u{1fbd}', ""); // GREEK KORONIS
    s = s.replace('\u{02BC}', ""); // apostrophe
    s = s.replace('ϝ', "εωωω"); // digamma
    s = s.replace("'st", "st2");
    s = s.replace('\'', ""); // remove any other single quotes
    s = s.replace('ς', "σ"); // sort all words with medial sigma
    s
}

struct Processor<'a> {
    lexica: Vec<Lexicon<'a>>,
    index_writer: IndexWriter,
    db: AnyConnection,
    unique_hashmap: HashMap<String, u32>, // to add numbers to end of non-unique lemmata
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
            .execute(&mut **tx)
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
        let mut doc = TantivyDocument::default();
        doc.add_u64(word_id_field, item_count.try_into().unwrap());
        doc.add_text(lemma_field, lemma);
        doc.add_text(lexicon_field, lexicon_name);
        doc.add_text(
            def_field,
            //hgk_strip_diacritics(item_text_no_tags, 0xFFFFFFFF),
            item_text_no_tags
        );
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
        reader.config_mut().trim_text(false); //FIX ME: check docs, do we want true here?
        reader.config_mut().enable_all_checks(true);
        //reader.trim_text(false); //false to preserve whitespace

        let mut buf = Vec::new();

        let mut entry = LexEntryCollector::new();

        let mut in_orth_tag = false;
        let mut in_head_tag = false;
        let mut in_text_tag = false;
        let mut in_entry = false;

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
                        //do not include <head> tags which are not in entries:
                        // e.g. the letter head tags of Lewis & Short (latindico01.xml)
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
                                in_entry = true;
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
                            in_entry = false;
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
                            in_entry = false;
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
                    if in_head_tag && in_entry {
                        let lemma = e.unescape().unwrap().to_string();
                        // add numbers to end of non-unique lemmata
                        let count = match self.unique_hashmap.get(&lemma) {
                            Some(count) => count + 1,
                            None => 1,
                        };
                        entry.head.push_str(
                            format!(
                                "{}{}",
                                lemma,
                                if count > 1 {
                                    count.to_string()
                                } else {
                                    "".to_string()
                                }
                            )
                            .as_str(),
                        );
                        self.unique_hashmap.insert(lemma, count);
                    }
                    if in_orth_tag && in_entry {
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
        let query = "CREATE TABLE IF NOT EXISTS words (seq INTEGER PRIMARY KEY, lexicon TEXT, word TEXT, sortword TEXT, def TEXT) STRICT; \
        CREATE INDEX IF NOT EXISTS lexicon_idx ON words (lexicon); \
        CREATE INDEX IF NOT EXISTS sortword_idx ON words (sortword); \
        CREATE INDEX IF NOT EXISTS word_idx ON words (word);";

        let _res = sqlx::query(query).execute(&mut self.db).await;

        let query = "DELETE FROM words;";
        let _res = sqlx::query(query).execute(&mut self.db).await;

        let mut item_count: i32 = 0;

        for lex in self.lexica.clone() {
            if lex.pull {
                if !Path::new(&lex.dir_name).exists() {
                    println!("Cloning {}...", &lex.repo_url);

                    let _repo = match git2::Repository::clone(lex.repo_url, lex.dir_name) {
                        Ok(repo) => repo,
                        Err(e) => panic!("failed to clone: {}", e),
                    };
                } else if let Ok(repo) = git2::Repository::discover(lex.dir_name) {
                    //else pull: i.e. fetch and merge
                    let mut remote = repo.find_remote(lex.remote).unwrap();
                    let fetch_commit = do_fetch(&repo, &[lex.branch], &mut remote).unwrap();
                    let _ = do_merge(&repo, lex.branch, fetch_commit);
                }
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
            self.unique_hashmap.clear(); // clear for next lexicon
            println!("items: {}", item_count);
        }

        self.index_writer.commit().unwrap();

        let query = "VACUUM;";
        let _res = sqlx::query(query).execute(&mut self.db).await;
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
        branch: "master",
        remote: "origin",
        pull: true,
    };
    let ls = Lexicon {
        dir_name: "LewisShortLogeion/",
        file_name: "latindico",
        repo_url: "https://github.com/helmadik/LewisShortLogeion.git",
        start_rng: 1,
        end_rng: 25,
        name: "lewisshort",
        branch: "master",
        remote: "origin",
        pull: true,
    };
    let slater = Lexicon {
        dir_name: "SlaterPindar/",
        file_name: "pindar_dico",
        repo_url: "https://github.com/jeremymarch/SlaterPindar.git",
        //repo_url: "https://github.com/helmadik/SlaterPindar.git",
        start_rng: 1,
        end_rng: 24,
        name: "slater",
        branch: "main",
        remote: "origin",
        pull: false,
    };

    let index_path = "tantivy-datav4"; // TempDir::new()?;
    if Path::new(index_path).is_dir() {
        fs::remove_dir_all(index_path).unwrap();
    }
    fs::create_dir(index_path).unwrap();

    let text_field_indexing = TextFieldIndexing::default()
        .set_tokenizer("el_stem"); // Use the registered name
    //.set_index_option(IndexRecordOption::WithFreqsAndPositions);
    // let lemma_text_options = TextOptions::default()
    //     //.set_indexing_options(text_field_indexing)
    //     .set_stored();
    // let lex_text_options = TextOptions::default()
    //     //.set_indexing_options(text_field_indexing)
    //     .set_stored();
    let def_text_options = TextOptions::default()
        .set_indexing_options(text_field_indexing)
        .set_stored();

    let mut schema_builder = Schema::builder();
    let num_options = NumericOptions::default().set_stored().set_indexed();
    schema_builder.add_u64_field("word_id", num_options);
    schema_builder.add_text_field("lemma", STRING | FAST | STORED); //STORED // lemma is also in definition, so no need to index it separately
    //schema_builder.add_text_field("lexicon", lex_text_options); //TEXT | STORED
    schema_builder.add_text_field("lexicon", STRING | FAST | STORED); //doc.add_text(status, "active");
    schema_builder.add_text_field("definition", def_text_options); // TEXT | STORED
    let schema = schema_builder.build();

    let index = Index::create_in_dir(index_path, schema.clone())?;
    let en_stem_analyzer = TextAnalyzer::builder(SimpleTokenizer::default())
        //.filter(StopWordFilter::new(Language::Greek))
        .filter(LowerCaser)
        .filter(NoDiacritcs)
        .filter(Stemmer::new(Language::English))
        .build();

    index.tokenizers().register("el_stem", en_stem_analyzer);

    // let index = Index::create_in_ram(schema.clone());
    let index_writer: IndexWriter = index.writer(50_000_000)?;

    install_default_drivers();
    let conn = AnyConnection::connect("sqlite://dbv3.sqlite?mode=rwc").await?;

    let unique_hashmap = HashMap::new();

    let mut processor = Processor {
        lexica: vec![lsj, ls /* , slater */],
        index_writer,
        db: conn,
        unique_hashmap,
    };

    processor.start().await.unwrap();

    //let word_id_field = index.schema().get_field("word_id").unwrap();
    //let lemma_field = index.schema().get_field("lemma").unwrap();
    let lexicon_field = index.schema().get_field("lexicon").unwrap();
    let definition_field = index.schema().get_field("definition").unwrap();

    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommitWithDelay)
        .try_into()?;

    let searcher = reader.searcher();
    let query_parser = QueryParser::for_index(
        &index,
        //this vector contains default fields used if field is not specified in query
        vec![lexicon_field, definition_field],
    );

    match query_parser.parse_query("carry AND (lexicon:slater OR lexicon:lewisshort)") {
        Ok(query) => match searcher.search(&query, &TopDocs::with_limit(100)) {
            Ok(top_docs) => {
                for (_score, doc_address) in top_docs {
                    match searcher.doc::<TantivyDocument>(doc_address) {
                        Ok(_retrieved_doc) => println!("success"), //println!("{}", schema.to_json(&retrieved_doc)),
                        Err(e) => println!("Error retrieving document: {:?}", e),
                    }
                }
            }
            Err(e) => println!("Error searching tantivy index: {:?}", e),
        },
        Err(e) => {
            println!("Query parsing error: {:?}", e);
        }
    }

    Ok(())
}

fn do_fetch<'a>(
    repo: &'a git2::Repository,
    refs: &[&str],
    remote: &'a mut git2::Remote,
) -> Result<git2::AnnotatedCommit<'a>, git2::Error> {
    let mut cb = git2::RemoteCallbacks::new();

    // Print out our transfer progress.
    cb.transfer_progress(|stats| {
        if stats.received_objects() == stats.total_objects() {
            print!(
                "Resolving deltas {}/{}\r",
                stats.indexed_deltas(),
                stats.total_deltas()
            );
        } else if stats.total_objects() > 0 {
            print!(
                "Received {}/{} objects ({}) in {} bytes\r",
                stats.received_objects(),
                stats.total_objects(),
                stats.indexed_objects(),
                stats.received_bytes()
            );
        }
        io::stdout().flush().unwrap();
        true
    });

    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(cb);
    // Always fetch all tags.
    // Perform a download and also update tips
    fo.download_tags(git2::AutotagOption::All);
    println!(
        "Fetching {} for {}",
        remote.name().unwrap(),
        repo.path().display()
    );
    remote.fetch(refs, Some(&mut fo), None)?;

    // If there are local objects (we got a thin pack), then tell the user
    // how many objects we saved from having to cross the network.
    let stats = remote.stats();
    if stats.local_objects() > 0 {
        println!(
            "\rReceived {}/{} objects in {} bytes (used {} local \
             objects)",
            stats.indexed_objects(),
            stats.total_objects(),
            stats.received_bytes(),
            stats.local_objects()
        );
    } else {
        println!(
            "\rReceived {}/{} objects in {} bytes",
            stats.indexed_objects(),
            stats.total_objects(),
            stats.received_bytes()
        );
    }

    let fetch_head = repo.find_reference("FETCH_HEAD")?;
    repo.reference_to_annotated_commit(&fetch_head)
}

fn fast_forward(
    repo: &Repository,
    lb: &mut git2::Reference,
    rc: &git2::AnnotatedCommit,
) -> Result<(), git2::Error> {
    let name = match lb.name() {
        Some(s) => s.to_string(),
        None => String::from_utf8_lossy(lb.name_bytes()).to_string(),
    };
    let msg = format!("Fast-Forward: Setting {} to id: {}", name, rc.id());
    println!("{}", msg);
    lb.set_target(rc.id(), &msg)?;
    repo.set_head(&name)?;
    repo.checkout_head(Some(
        git2::build::CheckoutBuilder::default()
            // For some reason the force is required to make the working directory actually get updated
            // I suspect we should be adding some logic to handle dirty working directory states
            // but this is just an example so maybe not.
            .force(),
    ))?;
    Ok(())
}

fn normal_merge(
    repo: &Repository,
    local: &git2::AnnotatedCommit,
    remote: &git2::AnnotatedCommit,
) -> Result<(), git2::Error> {
    let local_tree = repo.find_commit(local.id())?.tree()?;
    let remote_tree = repo.find_commit(remote.id())?.tree()?;
    let ancestor = repo
        .find_commit(repo.merge_base(local.id(), remote.id())?)?
        .tree()?;
    let mut idx = repo.merge_trees(&ancestor, &local_tree, &remote_tree, None)?;

    if idx.has_conflicts() {
        println!("Merge conflicts detected...");
        repo.checkout_index(Some(&mut idx), None)?;
        return Ok(());
    }
    let result_tree = repo.find_tree(idx.write_tree_to(repo)?)?;
    // now create the merge commit
    let msg = format!("Merge: {} into {}", remote.id(), local.id());
    let sig = repo.signature()?;
    let local_commit = repo.find_commit(local.id())?;
    let remote_commit = repo.find_commit(remote.id())?;
    // Do our merge commit and set current branch head to that commit.
    let _merge_commit = repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &msg,
        &result_tree,
        &[&local_commit, &remote_commit],
    )?;
    // Set working tree to match head.
    repo.checkout_head(None)?;
    Ok(())
}

fn do_merge<'a>(
    repo: &'a Repository,
    remote_branch: &str,
    fetch_commit: git2::AnnotatedCommit<'a>,
) -> Result<(), git2::Error> {
    // 1. do a merge analysis
    let analysis = repo.merge_analysis(&[&fetch_commit])?;

    // 2. Do the appropriate merge
    if analysis.0.is_fast_forward() {
        println!("Doing a fast forward");
        // do a fast forward
        let refname = format!("refs/heads/{}", remote_branch);
        match repo.find_reference(&refname) {
            Ok(mut r) => {
                fast_forward(repo, &mut r, &fetch_commit)?;
            }
            Err(_) => {
                // The branch doesn't exist so just set the reference to the
                // commit directly. Usually this is because you are pulling
                // into an empty repository.
                repo.reference(
                    &refname,
                    fetch_commit.id(),
                    true,
                    &format!("Setting {} to {}", remote_branch, fetch_commit.id()),
                )?;
                repo.set_head(&refname)?;
                repo.checkout_head(Some(
                    git2::build::CheckoutBuilder::default()
                        .allow_conflicts(true)
                        .conflict_style_merge(true)
                        .force(),
                ))?;
            }
        };
    } else if analysis.0.is_normal() {
        // do a normal merge
        let head_commit = repo.reference_to_annotated_commit(&repo.head()?)?;
        normal_merge(repo, &head_commit, &fetch_commit)?;
    } else {
        println!("Nothing to do...");
    }
    Ok(())
}

use std::mem;

use tantivy::tokenizer::{Token, TokenFilter, TokenStream, Tokenizer};

/// Token filter that removes diacritics from terms.
#[derive(Clone)]
pub struct NoDiacritcs;

impl TokenFilter for NoDiacritcs {
    type Tokenizer<T: Tokenizer> = DiacriticFilter<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        DiacriticFilter {
            tokenizer,
            buffer: String::new(),
        }
    }
}

#[derive(Clone)]
pub struct DiacriticFilter<T> {
    tokenizer: T,
    buffer: String,
}

impl<T: Tokenizer> Tokenizer for DiacriticFilter<T> {
    type TokenStream<'a> = DiacriticTokenStream<'a, T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        self.buffer.clear();
        DiacriticTokenStream {
            tail: self.tokenizer.token_stream(text),
            buffer: &mut self.buffer,
        }
    }
}

pub struct DiacriticTokenStream<'a, T> {
    buffer: &'a mut String,
    tail: T,
}

// writes a lowercased version of text into output.
fn to_diacritic_free_unicode(text: &str, output: &mut String) {
    output.clear();
    output.reserve(50);
    // for c in text.chars() {
    //     // Contrary to the std, we do not take care of sigma special case.
    //     // This will have an normalizationo effect, which is ok for search.
    //     output.extend(c.to_lowercase());
    // }
    let stripped = hgk_strip_diacritics(text, 0xFFFFFFFF);
    output.push_str(&stripped);
}

impl<T: TokenStream> TokenStream for DiacriticTokenStream<'_, T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        // if self.token_mut().text.is_ascii() {
        //     // fast track for ascii.
        //     self.token_mut().text.make_ascii_lowercase();
        // } else {
            to_diacritic_free_unicode(&self.tail.token().text, self.buffer);
            mem::swap(&mut self.tail.token_mut().text, self.buffer);
            //}
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}
