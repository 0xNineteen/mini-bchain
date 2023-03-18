use anyhow::Result;
use ed25519_dalek::Keypair;
use mini_bchain::rpc::RPCClient;
use rand::rngs::OsRng;

use libp2p::futures::StreamExt;
use libp2p::gossipsub::Sha256Topic;
use libp2p::{
    gossipsub, identity, mdns, swarm::SwarmEvent, PeerId, Swarm,
};
use tarpc::client::Config;
use tarpc::context;
use tarpc::tokio_serde::formats::Json;

use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;
use tokio::select;
use tokio::time::interval;

use tracing::{info, debug};

use mini_bchain::structures::*;
use mini_bchain::network::*;

#[tokio::main]
pub async fn main() -> Result<()> { 
    tracing_subscriber::fmt::init();

    // gossip sub for now ...
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    info!("Local peer id: {local_peer_id}");

    // let pubtime = rand::thread_rng().gen_range(5, 10);
    // let pubtime = Duration::from_nanos(1);
    let pubtime = Duration::from_secs(5);
    let mut tick = interval(pubtime);
    info!("using pubtime {pubtime:?}");

    // server address 
    let server_addr = SocketAddr::from_str("[::1]:8888")?;
    let transport = tarpc::serde_transport::tcp::connect(server_addr, Json::default);
    let client = RPCClient::new(Config::default(), transport.await?).spawn();

    let tps_tick_seconds = 3;
    let mut tps_tick = interval(Duration::from_secs(tps_tick_seconds));

    let gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(local_key.clone()),
        gossipsub::Config::default(),
    )
    .unwrap();

    let mdns = mdns::async_io::Behaviour::new(mdns::Config::default(), local_peer_id)?;
    let behaviour = ChainBehaviour { gossipsub, mdns };

    // Set up an encrypted DNS-enabled TCP Transport over the Mplex protocol.
    let transport = libp2p::development_transport(local_key.clone()).await?;
    let mut swarm = Swarm::with_threadpool_executor(transport, behaviour, local_peer_id);
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    let mut counter: u128 = 0;

    loop { 
        select! {
            event = swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(ChainBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                    for (peer_id, _multiaddr) in list {
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                    }
                },
                _ => {}
            },
            _ = tps_tick.tick() => { 
                info!("tps: {}", counter / tps_tick_seconds as u128);
                counter = 0;

                let hash = client.get_head(context::current()).await?.unwrap();
                let block = client.get_block(context::current(), hash).await?;
                info!("got block: {:x?}", block.unwrap().header.block_hash);
            },
            _ = tick.tick() => {
                counter += 1;

                // todo: change into client with RPC (not gossip sub lol)
                let mut rng = OsRng{};
                let keypair = Keypair::generate(&mut rng);
                let transaction = Transaction {
                    address: keypair.public.to_bytes(),
                    amount: 420
                };
                let transaction = transaction.sign(&keypair);
                let bytes = bytemuck::bytes_of(&transaction);

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