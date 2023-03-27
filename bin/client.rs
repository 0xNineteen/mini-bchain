use anyhow::{Result};
use borsh::BorshSerialize;
use ed25519_dalek::Keypair;
use rand::rngs::OsRng;

use libp2p::futures::StreamExt;
use libp2p::gossipsub::Sha256Topic;
use libp2p::{
    gossipsub, identity, mdns, swarm::SwarmEvent, PeerId, Swarm,
};
use tarpc::context;
use std::time::Duration;
use tokio::select;
use tokio::time::interval;

use tracing_subscriber::{EnvFilter, fmt};
use tracing::{info, debug};

use mini_bchain::structures::*;
use mini_bchain::network::*;
use mini_bchain::peer_manager::*;

#[tokio::main]
pub async fn main() -> Result<()> { 
    let filter = EnvFilter::try_from("INFO")?
        .add_directive("tarpc::client=ERROR".parse()?);

    fmt()
        .with_env_filter(filter)
        .init();

    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    info!("Local peer id: {local_peer_id}");

    let pubtime = Duration::from_secs(2);
    let mut tick = interval(pubtime);
    info!("using pubtime {pubtime:?}");

    let tps_tick_seconds = 3;
    let mut tps_tick = interval(Duration::from_secs(tps_tick_seconds));

    // gossip sub for now ...
    let gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(local_key.clone()),
        gossipsub::Config::default(),
    )
    .unwrap();

    let mdns = mdns::async_io::Behaviour::new(mdns::Config::default(), local_peer_id)?;
    let behaviour = ChainBehaviour { gossipsub, mdns };

    let transport = libp2p::development_transport(local_key.clone()).await?;
    let mut swarm = Swarm::with_threadpool_executor(transport, behaviour, local_peer_id);
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    let mut counter: u128 = 0;

    let mut peer_manager = PeerManager::default();

    loop { 
        select! {
            event = swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(ChainBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                    for (peer_id, _) in list {
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                        peer_manager.add_peer(peer_id).await?;
                    }
                },
                _ => {}
            },
            _ = tps_tick.tick() => { 
                info!("tps: {}", counter / tps_tick_seconds as u128);
                counter = 0;

                for (peer, client) in peer_manager.iter() { 
                    let hash = client.get_head(context::current()).await?.unwrap();
                    let block = client.get_block_header(context::current(), hash).await?;
                    info!("peer: {peer:?} got block: {:x?}", block.unwrap().block_hash);
                }
            },
            _ = tick.tick() => {
                counter += 1;

                // todo: change into client with RPC (not gossip sub lol)
                let mut rng = OsRng{};
                let keypair = Keypair::generate(&mut rng);
                let transaction = Transaction::AccountTransaction(AccountTransaction {
                    pubkey: keypair.public.to_bytes(),
                    amount: 420
                });
                let transaction = transaction.sign(&keypair);
                let bytes = transaction.try_to_vec()?;

                let result = swarm.behaviour_mut()
                    .gossipsub
                    .publish(Sha256Topic::new(TRANSACTION_TOPIC), bytes);

                if let Err(e) = result {
                    debug!("tx publish err: {e:?}");
                }
            }, 
        }
    }
}