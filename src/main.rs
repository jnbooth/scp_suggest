extern crate regex;
extern crate reqwest;
extern crate select;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;

use regex::Regex;
use reqwest::Client;
use select::document::Document;
use select::node::Node;
use select::predicate::{Attr, Class, Name, Predicate, Text};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::{HashSet, HashMap};
use std::error::Error;
use std::fs::File;
use std::cmp::Ordering;
use std::str::FromStr;

const MAX_ARTICLE: u16 = 5000;

fn ignore_tag(x: &str) -> bool {
    [ "safe"
    , "euclid"
    , "keter"
    , "thaumiel"
    , "neutralized"
    , "esoteric-class"
    , "joke"
    , "archived"
    , "decommissioned"
    , "neutralized"
    , "supplement"
    , "experiment"
    , "collaboration"
    , "exploration"
    , "incident"
    , "interview"
    , "tale"
    , "collaboration"
    , "alive"
    , "sentient"
    ].contains(&x)
}

type IO<T> = Result<T, Box<Error>>;

#[derive(Clone, Deserialize, Serialize)]
struct Article {
    number: u16,
    title: String,
    tags: HashSet<u16>,
    up: HashSet<u16>,
    down: HashSet<u16>
}

#[derive(Clone, Deserialize, Serialize)]
struct Suggestions {
    i: u16,
    s: String,
    xs: Vec<[usize; 2]>
}

#[derive(Clone, Deserialize, Serialize)]
struct Cube {
    number: u16,
    title: String,
    cube: String
}

impl Article {
    fn simil(&self, other: &Article) -> f64 {
        let x_up = &self.up;
        let x_down = &self.down;
        let y_up = &other.up;
        let y_down = &other.down;
        let x_mag = (x_up | x_down).len() as f64;
        let y_mag = (y_up | y_down).len() as f64;
        let pos = (x_up & y_up).len() as f64;
        let neg = (&(x_up & y_down) | &(x_down & y_up)).len() as f64;
        (pos - neg) / (x_mag.sqrt() * y_mag.sqrt())
    }

    pub fn suggestions(&self, xs: &[Article]) -> Vec<(Article, f64)> {
        let mut sorted: Vec<(Article, f64)> = 
            xs.into_iter().map(|x| (x.clone(), self.simil(&x))).collect();
        sorted.sort_unstable_by(|(_, a), (_, b)| 
            a.partial_cmp(&b).unwrap_or(Ordering::Equal).reverse()
        );
        sorted
    }
}

struct Indexer {
    i: u16,
    db: HashMap<String, u16>
}
impl Indexer {
    pub fn new() -> Indexer {
        Indexer { i: 0, db: HashMap::new() }
    }
    pub fn get(&mut self, k: String) -> u16 {
        match self.db.get(&k) {
            Some(&v) => v,
            None => {
                let i = self.i;
                self.i += 1;
                self.db.insert(k, i);
                i
            }
        }
    }
}

fn parse_title(node: &Node) -> Option<(u16, String)> {
    let link = node.find(Name("a")).next()?;
    let link_href = link.attr("href")?;
    let name = node.find(Name("span")).nth(1).or_else(||node.find(Text).nth(1))?;
    let name_text = name.text();
    if link_href.to_lowercase().starts_with("/scp-") {
        let link_num = &link_href[5..];
        let num = link_num.parse().ok()?;
        match name_text.find("- ") {
            None => Some((num, name_text)),
            Some(i) => Some((num, name_text[i+2..].to_string()))
        }
    } else { 
        None 
    }
}

/// Obtains a webpage's Wikidot ID.
fn parse_id(doc: &str) -> Result<u32, <u32 as FromStr>::Err> {
    {
        match doc.find("WIKIREQUEST.info.pageId = ") {
            None => None,
            Some(start) => {
                let (_, after) = doc.split_at(start + 26);
                match after.find(";") {
                    None => None,
                    Some(end) => {
                        let (before, _) = after.split_at(end);
                        Some(before)
                    }
                }
            }
        }
    }.unwrap_or("").parse()
}

fn parse_tags(tags: &mut Indexer, doc: &Document) -> HashSet<u16> {
    doc
        .find(Class("page-tags").child(Name("a")))
        .map(|x| x.text())
        .filter(|x| !ignore_tag(x))
        .map(|x| tags.get(x))
        .collect()
}

// The first thousand instances of "1 x 1 x 1" etc.
fn cube_queries() -> Vec<Regex> {
    (1..1000)
        .filter_map(|x| Regex::new(&format!("({}['\"]*\\s*[xX]\\s*{}['\"]*\\s*[xX]\\s*{}I)", x, x, x)).ok())
        .collect()
}

// Looks for "1 x 1 x 1" etc.
fn find_cube(queries: &Vec<Regex>, doc: &Document) -> Option<String> {
    let mut active = false;
    for node in doc.find(Attr("id", "page-content").child(Name("p"))) {
        let text = node.text();
        if text.contains("Special Containment Procedures:") {
            active = true;
        } else if text.contains("Description:") {
            active = false;
        }
        if active {
            for query in queries {
                for cap in query.captures_iter(&text) {
                    return Some(cap[0].to_string())
                }
            }
        }
    }
    None
}

/// (upvoters, downvoters) from a webpage.
fn parse_votes(users: &mut Indexer, doc: Document) -> (HashSet<u16>, HashSet<u16>) {
    let mut up = HashSet::new();
    let mut down = HashSet::new();
    for node in doc.find(Name("a")) {
        let text = node.text();
        match text.find("<\\/a>") {
            None    => (),
            Some(0) => (),
            Some(i) => {
                let (before, after) = text.split_at(i);
                let name = before.to_string();
                if name == "" {}
                else if after.contains('+') { 
                    up.insert(users.get(name));
                } else if after.contains('-') {
                    down.insert(users.get(name));
                }
            }
        }
    }
    (up, down)
}

/// Requests names for all articles from the mainlist.
fn record_titles(client: &Client) -> IO<HashMap<u16, String>> {
    let mut titles = HashMap::new();
    let mut pages: Vec<String> = 
        (2..6)
            .map(|i| format!("http://scp-wiki.wikidot.com/scp-series-{}", i))
            .collect();
    pages.push("http://scp-wiki.wikidot.com/scp-series".to_string());
    for page in pages {
        let res = client.get(&page).send()?;
        let doc = Document::from_read(res)?;
        for node in doc.find(Class("series").descendant(Name("li"))) {
            if let Some((num, title)) = parse_title(&node) {
                titles.insert(num, title);
            }
        }
    }
    Ok(titles)
}

/// Obtains Suggestions for an Article.
fn suggest(xs: &[Article], x: &Article) -> Suggestions {
    println!("Suggesting: SCP-{}", x.number);
    let suggestions: Vec<[usize; 2]> = x
        .suggestions(&xs)
        .into_iter()
        .take(21)
        .filter(|(y, _)| y.number != x.number)
        .map(|(y, score)| [y.number as usize, (score * 10000.0) as usize])
        .collect();
    Suggestions { i: x.number, s: x.title.clone(), xs: suggestions }
}

/// Queries Wikidot for an article's HtML.
fn request_page(client: &Client, number: u16) -> IO<Document> {
    let scp = format!("SCP-{:03}", number);
    println!("Scraping: {}", scp);
    let res = client
        .get(&format!("http://scp-wiki.wikidot.com/{}", scp))
        .send()?;
    Ok(Document::from_read(res)?)
}

fn any_matches(queries: &Vec<Regex>, x: &str) -> bool {
    for query in queries {
        if query.is_match(x) {
            return true;
        }
    }
    false
}

fn request_cubes(client: &Client) -> IO<Vec<Cube>> {
    let titles = record_titles(&client)?;
    let len = titles.len();
    let queries = cube_queries();
    for x in &["5 x 5 x 5", "5x5x5", "3\" x 3\" x 3\"", "5X5 x 5"] {
        println!("{} matches: {}", x, any_matches(&queries, &x))
    }
    let mut cubes = Vec::new();
    for (number, title) in titles {
        match request_page(&client, number).ok().and_then(|page| find_cube(&queries, &page)) {
            None => (),
            Some(cube) => {
                println!("SCP-{} ({}): {}", number, title, cube);
                cubes.push(Cube { number, title, cube })
            }
        }
    }
    println!("{} / {} ({:.2}%)", cubes.len(), len, cubes.len() as f64 * 100.0 / len as f64);
    Ok(cubes)
}

/// Queries Wikidot for information about an article.
fn request_article(client: &Client, users: &mut Indexer, tag: &mut Indexer, number: u16, title: String) -> IO<Article> {
    let page = request_page(&client, number)?;
    let text = page.nth(0).unwrap().inner_html();
    let id = parse_id(&text)?;
    let rated = request_module(&client, "pagerate/WhoRatedPageModule", id)?;
    let tags = parse_tags(tag, &page);
    let (up, down) = parse_votes(users, rated);
    Ok(Article { number, title, tags, up, down })
}

/// POSTs a request to Wikidot's AJAX module connector.
fn request_module(client: &Client, module_name: &str, id: u32) -> IO<Document> {
    let res = client
        .post("http://scp-wiki.wikidot.com/ajax-module-connector.php")
        .form(&[
            ("moduleName", module_name),
            ("pageId", &id.to_string())
        ])
        .send()?;
    Ok(Document::from_read(res)?)
}

fn read_json<T: DeserializeOwned>(path: &str) -> IO<T> {
    let file = File::open(path)?;
    Ok(serde_json::from_reader(file)?)
}
fn write_json<T: Serialize>(path: &str, data: &T) -> IO<()> {
    let file = File::create(path)?;
    Ok(serde_json::to_writer(file, data)?)
}

fn record_votes(client: &Client, users: &mut Indexer, tags: &mut Indexer, titles: HashMap<u16, String>) -> IO<()> {
    let mut articles = Vec::new();
    for i in 2..MAX_ARTICLE {
        let scp = format!("SCP-{:03}", i);
        let title = titles.get(&i).unwrap_or(&scp).to_string();
        if let Ok(article) = request_article(&client, users, tags, i, title) {
            println!("Loading: {}", scp);
            articles.push(article);
        }
    }
    println!("All articles loaded.");
    write_json("data/articles.json", &articles)
}

fn record_cubes() -> IO<()> {
    let client = Client::new();
    let cubes = request_cubes(&client)?;
    write_json("data/cubes.json", &cubes)
}

fn scrape() -> IO<()> {
    let client = Client::new();
    let mut users = Indexer::new();
    let mut tags = Indexer::new();
    let titles = record_titles(&client)?;
    record_votes(&client, &mut users, &mut tags, titles)
}

fn suggests() -> IO<()> {
    let articles: Vec<Article> = read_json("data/articles.json")?;
    let suggestions: Vec<Suggestions> = articles
        .clone()
        .into_iter()
        .map(|x| suggest(&articles, &x))
        .collect();
    write_json("data/suggestions.json", &suggestions)
}

fn scratch() -> Result<String, reqwest::Error> {
    Client::new()
        .post("http://scp-wiki.wikidot.com/ajax-module-connector.php")
        .form(&[
            ("moduleName", "list/ListPagesModule"),
            ("fullname", "SCP-3209")
        ])
        .send()?
        .text()
}

fn main() {
    match { scrape().and_then(|_| suggests()) } {
        Err(err) => println!("{}", err),
        Ok(_) => ()
    }
}
