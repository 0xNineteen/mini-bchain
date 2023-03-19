use std::{sync::{Arc}, net::{SocketAddr}, collections::{HashMap, hash_map::Iter}, str::FromStr};
use libp2p::{PeerId};
use tracing::info;

use crate::rpc::*;

#[derive(Default)]
pub struct PeerManager { 
    pub map: HashMap<PeerId, RPCClient>
}

impl PeerManager { 
    pub async fn add_peer(&mut self, peer_id: PeerId) -> anyhow::Result<()> { 
        let mut rpc_port = RPC_PORT_START; 
        loop { 
            // since I run nodes on a single machien 
            // we need RPCs to have different port numbers 
            // so we brute force to figure out what port this peer is on 

            let client = get_rpc_client(rpc_port, peer_id).await;
            if let Ok(client) = client { 
                info!("connected to {peer_id:?} on port {rpc_port:?}...");
                self.map.insert(peer_id, client);
                break;
            }
            rpc_port += 1;

            if rpc_port > RPC_PORT_START + 10 { 
                info!("peer id: {peer_id:?} rpc server port not found (likely client) ...");
                return Ok(())
            }
        }

        Ok(())
    }

    pub fn get_peer(&self, peer_id: &PeerId) -> Option<&RPCClient> { 
        self.map.get(peer_id)
    }

    pub fn iter(&self) -> Iter<'_, PeerId, RPCClient> { 
        self.map.iter()
    }
}