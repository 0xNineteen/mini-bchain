use std::{sync::Arc};

use tarpc::context::Context;

use crate::{structures::{Sha256Bytes, Block}, db::{RocksDB}};

#[tarpc::service]
pub trait RPC { 
    async fn get_block(hash: Sha256Bytes) -> Option<Block>;
}

#[derive(Clone)]
struct Server { 
    db: Arc<RocksDB>
}

#[tarpc::server]
impl RPC for Server { 
    async fn get_block(self, _: Context, block_hash: Sha256Bytes) -> Option<Block> { 
        self.db.get(block_hash).map(Some).unwrap_or(None)
    }
}

#[cfg(test)] 
mod tests { 
    use std::sync::Arc;
    use tarpc::{server::{self, Channel}, client::Config, context};
    use crate::{get_tmp_ledger_path_auto_delete, db::RocksDB};
    use rocksdb::DB;

    use super::*;

    #[tokio::test]
    async fn test_block_lookup() -> anyhow::Result<()> { 
        let path = get_tmp_ledger_path_auto_delete!();
        let db = DB::open_default(path).unwrap();
        let db = Arc::new(RocksDB { db });
        let _server = Server { 
            db: db.clone()
        };

        let genesis = Block::genesis(); 
        // no db insertion

        let (client_transport, server_transport) = tarpc::transport::channel::unbounded();
        let server = server::BaseChannel::with_defaults(server_transport);
        tokio::spawn({
            server.execute(_server.serve())
        });

        let client = RPCClient::new(Config::default(), client_transport).spawn();

        let block = client.get_block(context::current(), genesis.header.block_hash).await?;
        assert!(block.is_none());

        // block is now in db (should be rpc servable)
        db.put(&genesis)?;

        let block = client.get_block(context::current(), genesis.header.block_hash).await?;
        assert!(block.is_some());
        let rpc_block = block.unwrap(); 
        assert_eq!(rpc_block.header.block_hash, genesis.header.block_hash);

        Ok(())
    }

}