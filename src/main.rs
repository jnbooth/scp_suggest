// use std::collections::HashSet;
extern crate reqwest;
extern crate select;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;

use reqwest::Client;
use select::document::Document;
use select::node::Node;
use select::predicate::{Class, Name, Predicate, Text};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::{HashSet, HashMap};
use std::error::Error;
use std::fs::File;
use std::cmp::Ordering;
use std::str::FromStr;

const SCRAPE: bool = false;
const MAX_ARTICLE: u16 = 5000;

type ResultAny<T> = Result<T, Box<Error>>;

#[derive(Clone, Deserialize, Serialize)]
struct Article {
    number: u16,
    title: String,
    up: HashSet<u32>,
    down: HashSet<u32>
}

impl Article {
    fn simil(&self, other: &Article) -> f64 {
        let x_up = &self.up;
        let x_down = &self.down;
        let x_mag = (x_up | x_down).len() as f64;
        let y_up = &other.up;
        let y_down = &other.down;
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

struct Users {
    i: u32,
    users: HashMap<String, u32>
}
impl Users {
    pub fn new() -> Users {
        Users { i: 0, users: HashMap::new() }
    }
    pub fn get(&mut self, name: String) -> u32 {
        match self.users.get(&name) {
            Some(&user) => user,
            None => {
                let i = self.i;
                self.i += 1;
                self.users.insert(name, i);
                i
            }
        }
    }
}

fn record_titles(client: &Client) -> ResultAny<HashMap<u16, String>> {
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

/// Queries Wikidot for information about an article. Impure.
fn request_article(client: &Client, users: &mut Users, number: u16, title: String) -> ResultAny<Article> {
    let res = client
        .get(&format!("http://scp-wiki.wikidot.com/SCP-{:03}", number))
        .send()?
        .text()?;
    let id = parse_id(&res)?;
    let doc = request_module(&client, "pagerate/WhoRatedPageModule", id)?;
    Ok(parse_votes(users, number, title, doc))
}

/// POSTs a request to Wikidot's AJAX module connector. Impure.
fn request_module(client: &Client, module_name: &str, id: u32) -> ResultAny<Document> {
    let res = client
        .post("http://scp-wiki.wikidot.com/ajax-module-connector.php")
        .form(&[
            ("moduleName", module_name),
            ("pageId", &id.to_string())
        ])
        .send()?;
    Ok(Document::from_read(res)?)
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

/// Obtains upvoters and downvoters from a webpage and converts them into an Article.
fn parse_votes(users: &mut Users, number: u16, title: String, doc: Document) -> Article {
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
    Article { number, title, up, down }
}

fn read_json<T: DeserializeOwned>(path: &str) -> ResultAny<T> {
    let file = File::open(path)?;
    Ok(serde_json::from_reader(file)?)
}
fn write_json<T: Serialize>(path: &str, data: &T) -> ResultAny<()> {
    let file = File::create(path)?;
    Ok(serde_json::to_writer(file, data)?)
}

fn record_votes(client: &Client, users: &mut Users, titles: HashMap<u16, String>) -> ResultAny<()> {
    let mut articles = Vec::new();
    for i in 2..MAX_ARTICLE {
        let scp = format!("SCP-{:03}", i);
        let title = titles.get(&i).unwrap_or(&scp).to_string();
        if let Ok(article) = request_article(&client, users, i, title) {
            println!("Loading: {}", scp);
            articles.push(article);
        }
    }
    println!("All articles loaded.");
    write_json("data/articles.json", &articles)
}

fn scrape() -> ResultAny<()> {
    let client = reqwest::Client::new();
    let mut users = Users::new();
    let titles = record_titles(&client)?;
    record_votes(&client, &mut users, titles)
}

fn suggest() -> ResultAny<()> {
    let articles: Vec<Article> = read_json("data/articles.json")?;
    let it = articles
        .clone()
        .into_iter()
        .filter(|x| x.number == 1135)
        .flat_map(|x| x.suggestions(&articles))
        .take(10);
    for (article, score) in it {
        println!("{:.2}: {} - {}", score * 100.0, article.number, article.title);
    }
    Ok(())
}

fn main() {
    match if SCRAPE { scrape() } else { suggest() } {
        Err(err) => println!("{}", err),
        Ok(_) => ()
    }
}
