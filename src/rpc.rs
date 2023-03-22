use std::{sync::{Arc}, net::{SocketAddr}, str::FromStr};
use libp2p::PeerId;
use tokio::sync::Mutex;

use tarpc::{context::{Context, self}, tokio_serde::formats::Json, client::Config};
use tracing::info;

use crate::{structures::{Sha256Bytes, Block}, db::{RocksDB}, fork_choice::ForkChoice, machine::ChainState};
use tarpc::{server::{self, Channel}};
use futures::{future, prelude::*};
use anyhow::anyhow;

pub const RPC_PORT_START: u128 = 8888;

// todo: change to use bytemuck Codec (not json/serde)
#[tarpc::service]
pub trait RPC { 
    async fn get_block(hash: Sha256Bytes) -> Option<Block>;
    async fn get_head() -> Option<Sha256Bytes>;
    // used to find localnodes
    async fn get_peer_id() -> PeerId;
}

#[derive(Clone)]
struct Server { 
    db: Arc<RocksDB>,
    fork_choice: Arc<Mutex<ForkChoice>>,
    peer_id: PeerId
}

#[tarpc::server]
impl RPC for Server { 
    async fn get_block(self, _: Context, block_hash: Sha256Bytes) -> Option<Block> { 
        self.db.get(block_hash).map(Some).unwrap_or(None)
    }

    async fn get_head(self, _: Context) -> Option<Sha256Bytes> { 
        self.fork_choice.lock().await.get_head() // safe to unwrap here
    }

    async fn get_peer_id(self, _: Context) -> PeerId { 
        self.peer_id
    }
}

// problem: need 
    // 1) unique port/ip address for each node on gossipsub
    // 2) need unique port for the rpc

pub async fn rpc(
    chain_state: ChainState, 
) -> anyhow::Result<()> {

    let ChainState {
        fork_choice, 
        db, 
        keypair
    } = chain_state; 

    let local_peer_id = PeerId::from(keypair.public());

    let _server = Server { 
        db,
        fork_choice,
        peer_id: local_peer_id
    };

    // find available port
    let mut port = RPC_PORT_START;
    let mut listener;
    loop { 
        let server_addr = SocketAddr::from_str(&format!("[::1]:{}", port))?;
        listener = tarpc::serde_transport::tcp::listen(&server_addr, Json::default).await;
        if listener.is_ok() { 
            break; 
        } 
        port += 1;
    }
    let mut listener = listener.unwrap();
    info!("Listening on {}", listener.local_addr());

    listener.config_mut().max_frame_length(usize::MAX);
    listener
        // Ignore accept errors.
        .filter_map(|r| future::ready(r.ok()))
        .map(server::BaseChannel::with_defaults)
        .map(|channel| {
            let server = _server.clone();
            channel.execute(server.serve())
        })
        .buffer_unordered(100)
        .for_each(|_| async {})
        .await;

    Ok(())
}


// rpc client stuff
pub async fn get_rpc_client(port: u128, peer_id: PeerId) -> anyhow::Result<RPCClient> { 
    let server_addr = SocketAddr::from_str(&format!("[::1]:{}", port))?;
    let transport = tarpc::serde_transport::tcp::connect(server_addr, Json::default);
    let client = RPCClient::new(Config::default(), transport.await?).spawn();

    // verify it matches 
    let result = client.get_peer_id(context::current()).await?;
    if result == peer_id { 
        Ok(client)
    } else { 
        Err(anyhow!("incorrect peer id"))
    }
}


#[cfg(test)] 
mod tests { 
    use std::sync::Arc;
    use libp2p::identity;
    use tarpc::{server::{self, Channel}, client::Config, context};
    use crate::{get_tmp_ledger_path_auto_delete, db::RocksDB};
    use rocksdb::DB;

    use super::*;

    #[tokio::test]
    async fn test_block_lookup() -> anyhow::Result<()> { 
        let keypair = identity::Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(keypair.public());

        let path = get_tmp_ledger_path_auto_delete!();
        let db = DB::open_default(path).unwrap();
        let db = Arc::new(RocksDB { db });

        let fc = db.insert_genesis().unwrap();
        let fc = Arc::new(Mutex::new(fc));

        let _server = Server { 
            db: db.clone(),
            fork_choice: fc.clone(),
            peer_id: local_peer_id,
        };

        let genesis = Block::genesis(); 

        let (client_transport, server_transport) = tarpc::transport::channel::unbounded();
        let server = server::BaseChannel::with_defaults(server_transport);
        tokio::spawn({
            server.execute(_server.serve())
        });

        let client = RPCClient::new(Config::default(), client_transport).spawn();
        
        // block exists
        let block = client.get_block(context::current(), genesis.header.block_hash).await?;
        assert!(block.is_some());
        let rpc_block = block.unwrap(); 
        assert_eq!(rpc_block.header.block_hash, genesis.header.block_hash);

        // block dne
        let mut new_block = Block::genesis();
        new_block.header.nonce = 10; 
        new_block.header.commit_block_hash();

        let block = client.get_block(context::current(), new_block.header.block_hash).await?;
        assert!(block.is_none());

        Ok(())
    }

}