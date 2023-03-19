use anyhow::Result;
use libp2p::PeerId;
use libp2p::identity;
use mini_bchain::db::RocksDB;
use mini_bchain::machine::ChainState;
use mini_bchain::rpc::rpc;
use rand::Rng;
use rocksdb::DB;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use std::sync::Arc;

use tokio::runtime::Builder;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::Mutex;
use tracing::{info, Instrument};

use mini_bchain::network::*;
use mini_bchain::pow::*;

pub fn main() -> Result<()> {
    let filter = EnvFilter::try_from("INFO")?
        .add_directive("tarpc::client=ERROR".parse()?)
        .add_directive("tarpc::server=ERROR".parse()?);

    fmt()
        .with_env_filter(filter)
        .init();

    let runtime = Builder::new_multi_thread().enable_all().build().unwrap();

    // init p2p key
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(keypair.public());
    info!("Local peer id: {peer_id}");

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

    let chain_state = ChainState { 
        db, 
        fork_choice, 
        keypair
    };

    runtime.block_on(async move {
        let (p2p_tx_sender, p2p_tx_reciever) = unbounded_channel(); // p2p => producer
        let (producer_block_sender, producer_block_reciever) = unbounded_channel(); // producer => p2p

        // begin
        let state_ = chain_state.clone();
        let h1 = tokio::spawn(async move {
            block_producer(p2p_tx_reciever, producer_block_sender, state_)
                .instrument(tracing::info_span!("block producer"))
                .await
                .unwrap()
        });

        let state_ = chain_state.clone();
        let h2 = tokio::spawn(async move {
            network(p2p_tx_sender, producer_block_reciever, state_)
                .instrument(tracing::info_span!("network"))
                .await
                .unwrap()
        });

        let state_ = chain_state.clone();
        let h3 = tokio::spawn(async move { 
            rpc(state_)
                .instrument(tracing::info_span!("rpc"))
                .await
                .unwrap()
        });

        // should never finish
        h3.await.unwrap();
        h1.await.unwrap();
        h2.await.unwrap();
    });

    Ok(())
}