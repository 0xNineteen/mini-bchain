use anyhow::Result;
use mini_bchain::db::RocksDB;
use mini_bchain::rpc::rpc;
use rand::Rng;
use rocksdb::DB;
use std::sync::Arc;

use tokio::runtime::Builder;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::Mutex;
use tracing::{info, Instrument};

use mini_bchain::network::*;
use mini_bchain::pow::*;

pub fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let runtime = Builder::new_multi_thread().enable_all().build().unwrap();

    let port = 8888;

    // init db
    let id = rand::thread_rng().gen_range(0, 100);
    let path = format!("./db/db_{id}/");
    info!("using id: {id}");

    let db = DB::open_default(path).unwrap();
    let db = RocksDB { db };
    let db = Arc::new(db);

    // setup fork choice with genesis
    let fork_choice = db.insert_genesis().unwrap();
    let fork_choice = Arc::new(Mutex::new(fork_choice));

    runtime.block_on(async move {
        let (p2p_tx_sender, p2p_tx_reciever) = unbounded_channel(); // p2p => producer
        let (producer_block_sender, producer_block_reciever) = unbounded_channel(); // producer => p2p

        // begin
        let fc_ = fork_choice.clone();
        let db_ = db.clone();
        let h1 = tokio::spawn(async move {
            block_producer(p2p_tx_reciever, producer_block_sender, fc_, db_)
                .instrument(tracing::info_span!("block producer"))
                .await
                .unwrap()
        });

        let fc_ = fork_choice.clone();
        let db_ = db.clone();
        let h2 = tokio::spawn(async move {
            network(p2p_tx_sender, producer_block_reciever, fc_, db_)
                .instrument(tracing::info_span!("network"))
                .await
                .unwrap()
        });

        let fc_ = fork_choice.clone();
        let db_ = db.clone();
        let h3 = tokio::spawn(async move { 
            rpc(db_, fc_, port)
                .instrument(tracing::info_span!("rpc"))
                .await
                .unwrap()
        });

        // should never finish
        h1.await.unwrap();
        h2.await.unwrap();
        h3.await.unwrap();
    });

    Ok(())
}