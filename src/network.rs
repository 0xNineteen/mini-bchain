use std::ops::Deref;
use std::sync::Arc;
use std::vec;
use anyhow::{Result, anyhow};

use libp2p::futures::StreamExt;
use libp2p::gossipsub::Sha256Topic;
use libp2p::identity::Keypair;
// use libp2p::{quic, dns, websocket, Transport};
use libp2p::{
    gossipsub, identity, mdns, swarm::NetworkBehaviour, swarm::SwarmEvent, PeerId, Swarm,
};
use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};
use tarpc::context;
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use tracing::info;

use crate::fork_choice::ForkChoice;
use crate::machine::*;
use crate::structures::*;

use crate::rpc::*;
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
pub async fn network(
    p2p_tx_sender: UnboundedSender<SignedTransaction>,
    mut producer_block_reciever: UnboundedReceiver<Block>,
    chain_state: ChainState, 
) -> Result<()> {
    let ChainState {
        fork_choice, 
        db, 
        keypair
    } = chain_state; 

    let local_peer_id = PeerId::from(keypair.public());

    let mut gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(keypair.clone()),
        gossipsub::Config::default(),
    )
    .unwrap();

    for topic in GOSSIP_CORE_TOPICS {
        let topic = Sha256Topic::new(topic);
        gossipsub.subscribe(&topic)?;
    }

    let mdns = mdns::async_io::Behaviour::new(mdns::Config::default(), local_peer_id)?;
    let behaviour = ChainBehaviour { gossipsub, mdns };

    let transport = libp2p::development_transport(keypair.clone()).await?;

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
    let multi_addr = "/ip4/0.0.0.0/tcp/0".parse()?; 
    swarm.listen_on(multi_addr)?;

    let mut peer_manager = PeerManager::default();

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
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                        peer_manager.add_peer(peer_id).await?;
                    }
                },
                SwarmEvent::Behaviour(ChainBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                    message,
                    propagation_source, 
                    ..
                 })) => {
                    let data = message.data.as_slice();

                    if let Ok(tx) = bytemuck::try_from_bytes::<SignedTransaction>(data) {
                        // send to mempool
                        if tx.verify().is_ok() { 
                            p2p_tx_sender.send(*tx)?; // clone :(
                        }
                        continue;
                    }

                    match bytemuck::try_from_bytes::<Block>(data) {
                        Ok(block) => {
                            
                            let mut blocks_to_commit = vec![
                                *block
                            ];

                            while !blocks_to_commit.is_empty() { 
                                let block = blocks_to_commit.pop().unwrap();

                                let block_processing_result = process_network_block(
                                    &block, 
                                    db.clone(), 
                                    fork_choice.clone()
                                ).await;

                                // do repair 
                                if let Err(e) = block_processing_result { 

                                    // todo: fix error checking 
                                    let parent_hash = block.header.parent_hash;
                                    let missing_parent = {
                                        let fc = fork_choice.lock().await;
                                        !fc.exists(parent_hash)
                                    };

                                    if missing_parent { 
                                        info!("repairing missing parent block ...");
                                        let client = peer_manager.get_peer(&propagation_source).unwrap();
                                        // request missing parent block 
                                        let block = client.get_block(context::current(), parent_hash).await?.unwrap();
                                        blocks_to_commit.push(block);
                                    } else { 
                                        // if not missing parent then propogate it 
                                        return Err(e);
                                    }
                                } 
                            }
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

pub async fn process_network_block(
    block: &Block, 
    db: Arc<RocksDB>, 
    fork_choice: Arc<Mutex<ForkChoice>>,
) -> Result<()> { 
    let parent_hash = block.header.parent_hash;
    get_pinned!(db parent_hash => parent_block);

    // multithread sigverify
    let txs = &block.txs.0;
    let result = txs.par_iter().find_any(|tx| tx.verify().is_err());
    if result.is_some() { 
        return Err(anyhow!("block tx sig verification failed..."));
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
        return Err(anyhow!("block verification failed..."));
    }
    info!("block verification passed...");

    // store new block's state
    // todo: move behind forkchoice? 
    commit_new_block(block, account_digests, new_accounts, fork_choice.clone(), db.clone()).await?;

    Ok(())
}


