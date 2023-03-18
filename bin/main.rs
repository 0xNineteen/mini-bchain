use anyhow::Result;
use mini_bchain::db::RocksDB;
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

    // init db
    let id = rand::thread_rng().gen_range(0, 100);
    info!("using id: {id}");
    let path = format!("./db/db_{id}/");

    let db = DB::open_default(path).unwrap();
    let db = RocksDB { db };
    let db = Arc::new(db);

    runtime.block_on(async move {
        let (p2p_tx_sender, p2p_tx_reciever) = unbounded_channel(); // p2p => producer
        let (producer_block_sender, producer_block_reciever) = unbounded_channel(); // producer => p2p

        // setup fork choice with genesis
        let fork_choice = db.insert_genesis().unwrap();
        let fork_choice = Arc::new(Mutex::new(fork_choice));

        // begin
        let fc_ = fork_choice.clone();
        let db_ = db.clone();
        let h1 = tokio::spawn(async move {
            block_producer(p2p_tx_reciever, producer_block_sender, fc_, db_)
                .instrument(tracing::info_span!("block producer"))
                .await
                .unwrap()
        });

        let h2 = tokio::spawn(async move {
            network(p2p_tx_sender, producer_block_reciever, fork_choice, db)
                .instrument(tracing::info_span!("network"))
                .await
                .unwrap()
        });

        // todo: rpc thread

        // should never finish
        h1.await.unwrap();
        h2.await.unwrap();
    });

    Ok(())
}