use std::collections::HashMap;
use std::env;
use std::fmt::format;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::{Sender, Receiver};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time;
use warp::{Filter, Reply};
use warp::http::{StatusCode};

struct CRNAddEntry {
    crn: i64,
    handle: JoinHandle<anyhow::Result<()>>
}

struct CRNCheckResult {
    next_idx: usize,
    crn: i64,
    finished: bool
}

struct CRNCheckItem {
    idx: usize,
    sender: tokio::sync::oneshot::Sender<CRNCheckResult>
}

enum HandleOperation {
    Add(CRNAddEntry),
    Remove(i64),
    GetHandle(CRNCheckItem),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = env::args().collect();
    let path = args.remove(0);
    let path2 = path.clone();
    let path3 = path.clone();
    let (crn_tx, mut crn_rx) = tokio::sync::mpsc::channel(128);
    let crn_tx2 = crn_tx.clone();
    tokio::spawn(async move {
        warp_runner(crn_tx.clone(), path.clone()).await
    });
    tokio::spawn(async move {
        warp_scheduler(crn_tx2.clone(), path2.clone()).await
    });
    let mut crn_mapping = read_saved(path3.clone()).await;
    println!("CRN: {:?}", crn_mapping);
    while let Some(operation) = crn_rx.recv().await {
        match operation {
            HandleOperation::Add(add_entry) => {
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

async fn warp_runner(mod_tx: Sender<HandleOperation>, path: String) {
    let mod_tx2 = mod_tx.clone();
    let crn_addition = warp::path!("add" / i64).and_then(move |crn: i64| {
        let clone = mod_tx2.clone();
        let path = path.clone();
        async move {
            let resp = add_item(clone, crn, path).await;
            if resp.status() != 200 {
                Err(warp::reject::not_found())
            } else {
                Ok(resp)
            }
        }
    });

    let crn_removal = warp::path!("remove" / i64).and_then(move |crn: i64| {
        let clone = mod_tx.clone();
        async move {
            let resp = remove_item(clone, crn).await;
            if resp.status() != 200 {
                Err(warp::reject::not_found())
            } else {
                Ok(resp)
            }
        }
    });
    let routes = crn_addition.or(crn_removal);
    warp::serve(routes).run(([127, 0, 0, 1], 8080)).await;
}

async fn warp_scheduler(mod_tx: Sender<HandleOperation>, path: String) {
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
                        idx = item.next_idx;
                        interval.tick().await;
                    } else {
                        idx += 1;
                    }
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
    tokio::spawn(async move {
        let res = Command::new(format!("python3 {} Fall {}", path, crn))
            .output()
            .await?;
        println!("command out: {:?}", String::from_utf8(res.stdout));
        println!("command err: {:?}", String::from_utf8(res.stderr));
        Ok::<(), anyhow::Error>(())
    })
}

async fn add_item(mod_tx: Sender<HandleOperation>, crn: i64, path: String) -> warp::reply::Response {
    let handle = start_command(crn, path);

    match mod_tx.send(HandleOperation::Add(CRNAddEntry { crn, handle })).await {
        Ok(_) => warp::reply::with_status("", StatusCode::OK).into_response(),
        _ => warp::reply::with_status("Error generating handle", StatusCode::INTERNAL_SERVER_ERROR).into_response()
    }
}

async fn remove_item(mod_tx: Sender<HandleOperation>, crn: i64) -> warp::reply::Response {
    match mod_tx.send(HandleOperation::Remove(crn)).await {
        Ok(_) => warp::reply::with_status("", StatusCode::OK).into_response(),
        _ => warp::reply::with_status("Error sending remove handle", StatusCode::INTERNAL_SERVER_ERROR).into_response()
    }
}
