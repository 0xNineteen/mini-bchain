use std::ops::Deref;
use std::sync::Arc;
use std::vec;
use rand::Rng;

use anyhow::Result;
use ed25519_dalek::Keypair;
use rand::rngs::OsRng;

use libp2p::futures::StreamExt;
use libp2p::gossipsub::Sha256Topic;
use libp2p::{
    gossipsub, identity, mdns, swarm::NetworkBehaviour, swarm::SwarmEvent, PeerId, Swarm,
};

use std::time::Duration;
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::interval;

use tracing::info;

use crate::fork_choice::ForkChoice;
use crate::state::*;
use crate::structures::*;

const TRANSACTION_TOPIC: &str = "transactions";
const BLOCK_TOPIC: &str = "blocks";
const GOSSIP_CORE_TOPICS: [&str; 2] = [TRANSACTION_TOPIC, BLOCK_TOPIC];

#[derive(NetworkBehaviour)]
struct ChainBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::async_io::Behaviour,
}

pub async fn network<DB: ChainDB>(
    p2p_tx_sender: UnboundedSender<SignedTransaction>,
    mut producer_block_reciever: UnboundedReceiver<Block>,
    fork_choice: Arc<Mutex<ForkChoice>>,
    db: Arc<DB>,
) -> Result<()> {
    // Create a random PeerId
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    info!("Local peer id: {local_peer_id}");

    let mut gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(local_key.clone()),
        gossipsub::Config::default(),
    )
    .unwrap();

    for topic in GOSSIP_CORE_TOPICS {
        let topic = Sha256Topic::new(topic);
        gossipsub.subscribe(&topic)?;
    }

    let mdns = mdns::async_io::Behaviour::new(mdns::Config::default(), local_peer_id)?;
    let behaviour = ChainBehaviour { gossipsub, mdns };

    // Set up an encrypted DNS-enabled TCP Transport over the Mplex protocol.
    let transport = libp2p::development_transport(local_key.clone()).await?;
    let mut swarm = Swarm::with_threadpool_executor(transport, behaviour, local_peer_id);
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    let pubtime = rand::thread_rng().gen_range(5, 10);
    info!("using pubtime {pubtime:?}");
    let mut tick = interval(Duration::from_secs(pubtime));

    loop {
        select! {
            _ = tick.tick() => {
                // todo: change into client with RPC (not gossip sub lol)
                info!("publishing tx..");

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
                    info!("tx publish err: {e:?}");
                }
            }
            Some(block) = producer_block_reciever.recv() => {
                info!("publishing block...");

                let bytes = bytemuck::bytes_of(&block);
                let result = swarm.behaviour_mut()
                    .gossipsub
                    .publish(Sha256Topic::new(BLOCK_TOPIC), bytes);

                if let Err(e) = result {
                    info!("block publish err: {e:?}");
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
                    let data = message.data.as_slice();

                    match bytemuck::try_from_bytes::<SignedTransaction>(data) {
                        Ok(tx) => {
                            // send to mempool
                            info!("new p2p tx...");
                            p2p_tx_sender.send(*tx)?; // clone :(
                            continue;
                        },
                        Err(_) => {}
                    }


                    match bytemuck::try_from_bytes::<Block>(data) {
                        Ok(block) => {
                            info!("new p2p block ...");

                            let parent_hash = block.header.parent_hash;
                            get_pinned!(db parent_hash => parent_block);

                            let txs = &block.txs.0;

                            // re-produce the state change
                            let (
                                mut block_header,
                                account_digests,
                                new_accounts
                            ) = state_transition(parent_block, txs, db.clone())?;
                            block_header.nonce = block.header.nonce;
                            block_header.commit_block_hash();

                            // verify final block
                            let verification = {
                                block_header.block_hash == block.header.block_hash &&
                                block_header.state_root == block.header.state_root &&
                                block_header.tx_root == block.header.tx_root &&
                                block_header.is_valid_pow()
                            };
                            if !verification {
                                info!("block verification failed...");
                                continue;
                            }
                            info!("block verification passed...");

                            // store new block's state
                            // todo: move behind forkchoice? 
                            commit_new_block(&block, account_digests, new_accounts, fork_choice.clone(), db.clone()).await?;
                        },
                        Err(_) => {
                            info!("serialization failed...");
                        }
                    }
                }
                _ => { }
            }
        }
    }
}
