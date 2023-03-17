use std::ops::Deref;
use std::sync::Arc;
use std::vec;
use anyhow::Result;

use libp2p::futures::StreamExt;
use libp2p::gossipsub::Sha256Topic;
// use libp2p::{quic, dns, websocket, Transport};
use libp2p::{
    gossipsub, identity, mdns, swarm::NetworkBehaviour, swarm::SwarmEvent, PeerId, Swarm,
};
use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use tracing::info;

use crate::fork_choice::ForkChoice;
use crate::machine::*;
use crate::structures::*;

use crate::db::*;
use crate::get_pinned;

pub const TRANSACTION_TOPIC: &str = "transactions";
pub const BLOCK_TOPIC: &str = "blocks";
pub const GOSSIP_CORE_TOPICS: [&str; 2] = [TRANSACTION_TOPIC, BLOCK_TOPIC];

// todo: kalhmedia node lookup + non-local network
// todo: improve blockpropogation -- solana's turbine/bittorrent
#[derive(NetworkBehaviour)]
pub struct ChainBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::async_io::Behaviour,
}

// todo: include a state s.t if pow fails this auto-stops
// eg, use a shared (rwlock) enum Status::Failed(String)
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

    // // Set up an encrypted DNS-enabled TCP Transport over the Mplex protocol.
    let transport = libp2p::development_transport(local_key.clone()).await?;

    // // todo: get quicc working 
    // let transport = {
    //     let quic_config = quic::Config::new(&local_key);
    //     let dns_quic = dns::DnsConfig::system(
    //         quic::async_std::Transport::new(quic_config)
    //     ).await?;
    //     let ws_dns_quic = websocket::WsConfig::new(
    //         dns_quic
    //     );
    //     dns_quic.boxed()
    // };

    let mut swarm = Swarm::with_threadpool_executor(transport, behaviour, local_peer_id);
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    loop {
        select! {
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
                            if tx.verify().is_ok() { 
                                p2p_tx_sender.send(*tx)?; // clone :(
                            }
                            continue;
                        },
                        Err(_) => {}
                    }
                    match bytemuck::try_from_bytes::<Block>(data) {
                        Ok(block) => {
                            let parent_hash = block.header.parent_hash;
                            get_pinned!(db parent_hash => parent_block);

                            // multithread sigverify
                            let txs = &block.txs.0;
                            let result = txs.par_iter().find_any(|tx| tx.verify().is_err());
                            if result.is_some() { 
                                info!("block tx sig verification failed...");
                                continue;
                            }

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
