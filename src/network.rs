// client continuously sends txs 
// p2p collects transactions

use borsh::{BorshSerialize, BorshDeserialize};
use libp2p::futures::StreamExt;
use libp2p::gossipsub::{Sha256Topic};
use libp2p::{
    gossipsub, identity, mdns, swarm::NetworkBehaviour, swarm::SwarmEvent, PeerId, Swarm,
};
use tokio::select;
use tokio::time::interval;
use std::error::Error;
use std::time::Duration;

use tracing_subscriber;
use tracing::{info};

use mini_bchain::{Transaction, Block};


#[derive(NetworkBehaviour)]
struct ChainBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::async_io::Behaviour,
}

const TRANSACTION_TOPIC: &str = "transactions";
const BLOCK_TOPIC: &str = "blocks";
const GOSSIP_CORE_TOPICS: [&str; 2] = [
    TRANSACTION_TOPIC, 
    BLOCK_TOPIC 
];

#[derive(BorshSerialize, BorshDeserialize, Debug)]
enum Broadcast { 
    Transaction(Transaction), 
    Block(Block)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> { 
    tracing_subscriber::fmt::init();

    // Create a random PeerId
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    info!("Local peer id: {local_peer_id}");

    // Set up an encrypted DNS-enabled TCP Transport over the Mplex protocol.
    let transport = libp2p::development_transport(local_key.clone()).await?;

    let mut gossipsub = gossipsub::Behaviour::new( 
        gossipsub::MessageAuthenticity::Signed(local_key.clone()),
        gossipsub::Config::default()
    )?;


    for topic in GOSSIP_CORE_TOPICS { 
        let topic = Sha256Topic::new(topic);
        gossipsub.subscribe(&topic)?;
    }

    let mdns = mdns::async_io::Behaviour::new(mdns::Config::default(), local_peer_id)?;
    let behaviour = ChainBehaviour { 
        gossipsub, 
        mdns
    };

    let mut swarm = Swarm::with_threadpool_executor(transport, behaviour, local_peer_id);
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;
    let mut tick = interval(Duration::from_secs(4));

    loop {
        select! {
            _ = tick.tick() => { 
                info!("publishing...");

                let transaction = Transaction { 
                    address: [0; 32], 
                    amount: 420
                };
                let transaction = Broadcast::Transaction(transaction);
                let bytes = transaction.try_to_vec().unwrap();

                let result = swarm.behaviour_mut()
                    .gossipsub
                    .publish(Sha256Topic::new(TRANSACTION_TOPIC), bytes);

                if let Err(e) = result { 
                    info!("err: {e:?}");
                }
            }
            event = swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(ChainBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                    for (peer_id, _multiaddr) in list {
                        info!("mDNS discovered a new peer: {peer_id}");
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                    }
                },
                SwarmEvent::Behaviour(ChainBehaviourEvent::Gossipsub(gossipsub::Event::Message { 
                    message,
                    ..
                 })) => { 
                    let event = Broadcast::try_from_slice(message.data.as_slice()).unwrap();
                    match event { 
                        Broadcast::Transaction(tx) => {
                            info!("tx: {tx:?}");
                        }
                        Broadcast::BlockHeader(block) => {
                            info!("block: {block:?}");
                        }
                    }
                }
                _ => { }
            }
            
        }
    }
}

#[cfg(test)]
mod tests {

}