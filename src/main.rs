// # Basic Example
//
// This example covers the basic functionalities of
// tantivy.
//
// We will :
// - define our schema
// - create an index in a directory
// - index a few documents into our index
// - search for the best document matching a basic query
// - retrieve the best document's original content.

// ---
// Importing tantivy...
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{doc, Index, ReloadPolicy};
use tempfile::TempDir;

use quick_xml::events::Event;
use quick_xml::reader::Reader;

extern crate git2;
use git2::Repository;
use std::path::Path;
use std::fs;

use quick_xml::events::BytesStart;
use std::io::Cursor;
use quick_xml::Writer;
use quick_xml::events::BytesEnd;


fn read_xml(file:&str, item_count:&mut u32) {
    //println!("file: {}", file);
    let mut reader = Reader::from_file(file).unwrap();
    reader.trim_text(true);

    //let mut txt = Vec::new();
    let mut buf = Vec::new();
    //let mut item_text = String::new("");
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => panic!("Error at position {}: {:?}", reader.buffer_position(), e),
            Ok(Event::Eof) => break,
            Ok(Event::Comment(_e)) => {},
            Ok(Event::CData(_e)) => {},
            Ok(Event::Decl(_e)) => {},
            Ok(Event::PI(_e)) => {},
            Ok(Event::DocType(_e)) => {},

            Ok(Event::Start(e)) => {
                match e.name().as_ref() {
                    b"div1" => { 
                        writer = Writer::new(Cursor::new(Vec::new()));
                        let mut elem = BytesStart::new("div");
                        
                    },
                    b"div2" => { 
                        *item_count += 1;
                        writer = Writer::new(Cursor::new(Vec::new()));
                        let mut elem = BytesStart::new("div");
                    },
                    b"sense" => {  },
                    b"sense-1" => {  },
                    b"orth" => {  },
                    b"author" => {  },
                    b"quote" => {  },
                    b"foreign" => {  },
                    b"i" => {  },
                    b"title" => {  },
                    b"bibl" => {  },
                    _ => (),
                }
            },
            Ok(Event::End(e)) => {
                match e.name().as_ref() {
                    b"div1" => {  
                        writer.write_event(Event::End(BytesEnd::new("div")));
                        let result = writer.into_inner().into_inner();
                        println!("word: {:?}", result);
                    },
                    b"div2" => {
                        writer.write_event(Event::End(BytesEnd::new("div")));
                        let result = writer.into_inner().into_inner();
                        println!("word: {:?}", result);
                    },
                    b"sense" => {  },
                    b"sense-1" => {  },
                    b"orth" => {  },
                    b"author" => {  },
                    b"quote" => {  },
                    b"foreign" => {  },
                    b"i" => {  },
                    b"title" => {  },
                    b"bibl" => {  },
                    _ => (),
                }
            },
            Ok(Event::Empty(_e)) => {},
            Ok(Event::Text(e)) => {
                //txt.push(e.unescape().unwrap().into_owned())
                writer.write_event(Event::Text(e));
                //writer.write_text_content(e.unescape().unwrap())
            },
        }
        buf.clear();
    }
}

fn main() -> tantivy::Result<()> {
    let repo_path = "LSJLogeion/";
	let repo_url = "https://github.com/helmadik/LSJLogeion.git";
    let mut count = 0;
	
	if !Path::new(repo_path).exists() {
		println!("Cloning {}...", repo_url);

		//https://docs.rs/git2/0.16.1/git2/
		let _repo = match Repository::clone(repo_url, repo_path) {
		    Ok(repo) => repo,
		    Err(e) => panic!("failed to clone: {}", e),
		};
	}

	for entry in fs::read_dir(repo_path).expect("Unable to list") {
        let entry = entry.expect("unable to get entry");
        //println!( "{}", entry.path().display() );

        if let Some(path) = entry.path().to_str() {
            if path.ends_with(".xml") {
                read_xml(path, &mut count);
            }
        }
    }
    println!("items: {}", count);





    // Let's create a temporary directory for the
    // sake of this example
    let index_path = TempDir::new()?; //"tantivy-data"; //

    // # Defining the schema
    //
    // The Tantivy index requires a very strict schema.
    // The schema declares which fields are in the index,
    // and for each field, its type and "the way it should
    // be indexed".

    // First we need to define a schema ...
    let mut schema_builder = Schema::builder();

    // Our first field is title.
    // We want full-text search for it, and we also want
    // to be able to retrieve the document after the search.
    //
    // `TEXT | STORED` is some syntactic sugar to describe
    // that.
    //
    // `TEXT` means the field should be tokenized and indexed,
    // along with its term frequency and term positions.
    //
    // `STORED` means that the field will also be saved
    // in a compressed, row-oriented key-value store.
    // This store is useful for reconstructing the
    // documents that were selected during the search phase.
    schema_builder.add_text_field("title", TEXT | STORED);

    // Our second field is body.
    // We want full-text search for it, but we do not
    // need to be able to be able to retrieve it
    // for our application.
    //
    // We can make our index lighter by omitting the `STORED` flag.
    schema_builder.add_text_field("body", TEXT | STORED);

    let schema = schema_builder.build();

    // # Indexing documents
    //
    // Let's create a brand new index.
    //
    // This will actually just save a meta.json
    // with our schema in the directory.
    let index = Index::create_in_dir(&index_path, schema.clone())?;

    // To insert a document we will need an index writer.
    // There must be only one writer at a time.
    // This single `IndexWriter` is already
    // multithreaded.
    //
    // Here we give tantivy a budget of `50MB`.
    // Using a bigger memory_arena for the indexer may increase
    // throughput, but 50 MB is already plenty.
    let mut index_writer = index.writer(50_000_000)?;

    // Let's index our documents!
    // We first need a handle on the title and the body field.

    // ### Adding documents
    //
    // We can create a document manually, by setting the fields
    // one by one in a Document object.
    let title = schema.get_field("title").unwrap();
    let body = schema.get_field("body").unwrap();

    let mut old_man_doc = Document::default();
    old_man_doc.add_text(title, "ὅτι μὲν ὑμεῖς");
    old_man_doc.add_text(
        body,
        "ὅτι μὲν ὑμεῖς, ὦ ἄνδρες Ἀθηναῖοι, πεπόνθατε ὑπὸ τῶν ἐμῶν κατηγόρων, οὐκ οἶδα· ἐγὼ δʼ οὖν καὶ αὐτὸς ὑπʼ αὐτῶν ὀλίγου ἐμαυτοῦ ἐπελαθόμην, οὕτω πιθανῶς ἔλεγον. καίτοι ἀληθές γε ὡς ἔπος εἰπεῖν οὐδὲν εἰρήκασιν. μάλιστα δὲ αὐτῶν ἓν ἐθαύμασα τῶν πολλῶν ὧν ἐψεύσαντο, τοῦτο ἐν ᾧ ἔλεγον ὡς χρῆν ὑμᾶς εὐλαβεῖσθαι μὴ ὑπʼ ἐμοῦ ἐξαπατηθῆτε",
    );

    // ... and add it to the `IndexWriter`.
    index_writer.add_document(old_man_doc)?;

    // For convenience, tantivy also comes with a macro to
    // reduce the boilerplate above.
    index_writer.add_document(doc!(
    title => "ὡς δεινοῦ ὄντος λέγειν",
    body => "ὡς δεινοῦ ὄντος λέγειν. τὸ γὰρ μὴ αἰσχυνθῆναι ὅτι αὐτίκα ὑπʼ ἐμοῦ ἐξελεγχθήσονται ἔργῳ, ἐπειδὰν μηδʼ ὁπωστιοῦν φαίνωμαι δεινὸς λέγειν, τοῦτό μοι ἔδοξεν αὐτῶν ἀναισχυντότατον εἶναι, εἰ μὴ ἄρα δεινὸν καλοῦσιν οὗτοι λέγειν τὸν τἀληθῆ λέγοντα· εἰ μὲν γὰρ τοῦτο λέγουσιν, ὁμολογοίην ἂν ἔγωγε οὐ κατὰ τούτους εἶναι ῥήτωρ. οὗτοι μὲν οὖν, ὥσπερ ἐγὼ λέγω, ἤ τι ἢ οὐδὲν ἀληθὲς εἰρήκασιν, ὑμεῖς δέ μου ἀκούσεσθε πᾶσαν τὴν ἀλήθειαν—οὐ μέντοι μὰ Δία, ὦ ἄνδρες Ἀθηναῖοι, κεκαλλιεπημένους γε λόγους, ὥσπερ οἱ τούτων,"
    ))?;

    // Multivalued field just need to be repeated.
    index_writer.add_document(doc!(
    title => "Frankenstein",
    title => "The Modern Prometheus",
    body => "You will rejoice to hear that no disaster has accompanied the commencement of an \
             enterprise which you have regarded with such evil forebodings.  I arrived here \
             yesterday, and my first task is to assure my dear sister of my welfare and \
             increasing confidence in the success of my undertaking."
    ))?;

    // This is an example, so we will only index 3 documents
    // here. You can check out tantivy's tutorial to index
    // the English wikipedia. Tantivy's indexing is rather fast.
    // Indexing 5 million articles of the English wikipedia takes
    // around 3 minutes on my computer!

    // ### Committing
    //
    // At this point our documents are not searchable.
    //
    //
    // We need to call `.commit()` explicitly to force the
    // `index_writer` to finish processing the documents in the queue,
    // flush the current index to the disk, and advertise
    // the existence of new documents.
    //
    // This call is blocking.
    index_writer.commit()?;

    // If `.commit()` returns correctly, then all of the
    // documents that have been added are guaranteed to be
    // persistently indexed.
    //
    // In the scenario of a crash or a power failure,
    // tantivy behaves as if it has rolled back to its last
    // commit.

    // # Searching
    //
    // ### Searcher
    //
    // A reader is required first in order to search an index.
    // It acts as a `Searcher` pool that reloads itself,
    // depending on a `ReloadPolicy`.
    //
    // For a search server you will typically create one reader for the entire lifetime of your
    // program, and acquire a new searcher for every single request.
    //
    // In the code below, we rely on the 'ON_COMMIT' policy: the reader
    // will reload the index automatically after each commit.
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommit)
        .try_into()?;

    // We now need to acquire a searcher.
    //
    // A searcher points to a snapshotted, immutable version of the index.
    //
    // Some search experience might require more than
    // one query. Using the same searcher ensures that all of these queries will run on the
    // same version of the index.
    //
    // Acquiring a `searcher` is very cheap.
    //
    // You should acquire a searcher every time you start processing a request and
    // and release it right after your query is finished.
    let searcher = reader.searcher();

    // ### Query

    // The query parser can interpret human queries.
    // Here, if the user does not specify which
    // field they want to search, tantivy will search
    // in both title and body.
    let query_parser = QueryParser::for_index(&index, vec![title, body]);

    // `QueryParser` may fail if the query is not in the right
    // format. For user facing applications, this can be a problem.
    // A ticket has been opened regarding this problem.
    let query = query_parser.parse_query("λέγοντα")?;

    // A query defines a set of documents, as
    // well as the way they should be scored.
    //
    // A query created by the query parser is scored according
    // to a metric called Tf-Idf, and will consider
    // any document matching at least one of our terms.

    // ### Collectors
    //
    // We are not interested in all of the documents but
    // only in the top 10. Keeping track of our top 10 best documents
    // is the role of the `TopDocs` collector.

    // We can now perform our query.
    let top_docs = searcher.search(&query, &TopDocs::with_limit(10))?;

    // The actual documents still need to be
    // retrieved from Tantivy's store.
    //
    // Since the body field was not configured as stored,
    // the document returned will only contain
    // a title.
    for (_score, doc_address) in top_docs {
        let retrieved_doc = searcher.doc(doc_address)?;
        println!("{}", schema.to_json(&retrieved_doc));
    }

    Ok(())
}
