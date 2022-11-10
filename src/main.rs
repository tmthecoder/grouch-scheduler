use std::collections::HashMap;
use std::env;
use std::time::Duration;
use rocket::{Ignite, Rocket, routes, State};
use rocket::http::Status;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::{Sender, Receiver};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time;

#[macro_use] extern crate rocket;

#[derive(Debug)]
struct CRNAddEntry {
    crn: i64,
    handle: JoinHandle<anyhow::Result<()>>
}

#[derive(Debug)]
struct CRNCheckResult {
    next_idx: usize,
    crn: i64,
    finished: bool
}

#[derive(Debug)]
struct CRNCheckItem {
    idx: usize,
    sender: tokio::sync::oneshot::Sender<CRNCheckResult>
}

#[derive(Debug)]
enum HandleOperation {
    Add(CRNAddEntry),
    Remove(i64),
    GetHandle(CRNCheckItem),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = env::args().collect();
    let path = args.remove(1);
    let path2 = path.clone();
    let path3 = path.clone();
    let (crn_tx, mut crn_rx) = tokio::sync::mpsc::channel(128);
    let crn_tx2 = crn_tx.clone();
    tokio::spawn(async move {
        let _ = rocket_runner(crn_tx.clone(), path.clone()).await;
    });
    // tokio::spawn(async move {
    //     warp_scheduler(crn_tx2.clone(), path2.clone()).await
    // });
    let mut crn_mapping = read_saved(path3.clone()).await;
    while let Some(operation) = crn_rx.recv().await {
        println!("CRN Map: {:?}", crn_mapping, );
        println!("Current Op: {:?}", operation);
        match operation {
            HandleOperation::Add(add_entry) => {
                if crn_mapping.contains_key(&add_entry.crn) {
                    let removed = crn_mapping.remove(&add_entry.crn).unwrap();
                    removed.abort();
                }
                crn_mapping.insert(add_entry.crn, add_entry.handle);
            }

            HandleOperation::Remove(crn) => {
                let handle = crn_mapping.remove(&crn);
                if let Some(handle) = handle {
                    handle.abort();
                }
            }

            HandleOperation::GetHandle(check_item) => {
                let mut idx = check_item.idx;
                if idx >= crn_mapping.len() {
                    idx = 0;
                }
                let crn_entry = crn_mapping.iter().nth(check_item.idx).unwrap();

                if idx + 1 >= crn_mapping.len() {
                    idx = 0;
                } else {
                    idx += 1;
                }

                let crn_result = CRNCheckResult { next_idx: idx, crn: crn_entry.0.clone(), finished: crn_entry.1.is_finished() };
                let _ = check_item.sender.send(crn_result);
            }
        }
    }

    Ok(())
}

async fn rocket_runner(mod_tx: Sender<HandleOperation>, path: String) -> Rocket<Ignite> {
    rocket::build()
        .manage(mod_tx)
        .manage(path)
        .mount("/add", routes![add_crn])
        .mount("/remove", routes![remove_crn])
        .launch()
        .await
        .unwrap()
}
#[post("/add/<crn>")]
async fn add_crn(crn: i64, sender: &State<Sender<HandleOperation>>, path: &State<String>) -> (Status, String) {
    add_item((*sender).clone(), crn, (*path).clone()).await
}

#[post("/remove/<crn>")]
async fn remove_crn(crn: i64, sender: &State<Sender<HandleOperation>>) -> (Status, String) {
    remove_item((*sender).clone(), crn).await
}

async fn scheduler(mod_tx: Sender<HandleOperation>, path: String) {
    let mut interval = time::interval(Duration::from_millis(60000));
    let mut idx = 0;

    loop {
        let (check_tx, check_rx) = tokio::sync::oneshot::channel();
        let _ = mod_tx.send(HandleOperation::GetHandle( CRNCheckItem { idx, sender: check_tx } )).await;
        match check_rx.await {
            Ok(item) => {
                if item.finished {
                    let _ = mod_tx.send(HandleOperation::Remove(item.crn)).await;
                    let handle = start_command(item.crn, path.clone());
                    let _ = mod_tx.send(HandleOperation::Add( CRNAddEntry { crn: item.crn, handle } )).await;
                    if item.next_idx <= idx {
                        interval.tick().await;
                    }
                    idx = item.next_idx;
                }
            },
            _ => continue
        }
    }
}

async fn read_saved(path: String) -> HashMap<i64, JoinHandle<anyhow::Result<()>>> {
    let file = File::open("saved_crns.txt").await.unwrap();
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut map = HashMap::new();
    while let Some(line) = lines.next_line().await.unwrap() {
        let crn = line.parse::<i64>().unwrap();
        map.insert(crn, start_command(crn, path.clone()));
    }
    map
}

fn start_command(crn: i64, path: String) -> JoinHandle<anyhow::Result<()>> {
    let mut interval = time::interval(Duration::from_millis(60000));
    tokio::spawn(async move {
        let path = path.clone();
        loop {
            let path = path.clone();
            let res = Command::new("python3")
                .arg(path)
                .arg("spring")
                .arg(format!("{}", crn))
                .output()
                .await?;
            println!("command out: {:?}", String::from_utf8(res.stdout));
            println!("command err: {:?}", String::from_utf8(res.stderr));
            interval.tick().await;
        }
        Ok::<(), anyhow::Error>(())
    })
}

async fn add_item(mod_tx: Sender<HandleOperation>, crn: i64, path: String) -> (Status, String) {
    let handle = start_command(crn, path);

    match mod_tx.send(HandleOperation::Add(CRNAddEntry { crn, handle })).await {
        Ok(_) => (Status::Ok, "".into()),
        _ =>(Status::InternalServerError, "Error generating handle".into())
    }
}

async fn remove_item(mod_tx: Sender<HandleOperation>, crn: i64) -> (Status, String) {
    match mod_tx.send(HandleOperation::Remove(crn)).await {
        Ok(_) => (Status::Ok, "".into()),
        _ => (Status::NotFound, "Item not found or unexpected error occurred".into())
    }
}
