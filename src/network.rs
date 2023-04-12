use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;
use std::vec;
use anyhow::{Result, anyhow};

use borsh::{BorshDeserialize, BorshSerialize};
use libp2p::futures::StreamExt;
use libp2p::gossipsub::Sha256Topic;

// use libp2p::{quic, dns, websocket, Transport};
use libp2p::{
    gossipsub, mdns, swarm::NetworkBehaviour, swarm::SwarmEvent, PeerId, Swarm,
};
use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};
use solana_metrics::datapoint_info;
use tarpc::context;
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use tokio::time::interval;
use tracing::info;

use crate::fork_choice::ForkChoice;
use crate::machine::*;
use crate::rpc::RPCClient;
use crate::structures::*;

use crate::peer_manager::*;
use crate::db::*;
use crate::get_pinned;

pub const TRANSACTION_TOPIC: &str = "transactions";
pub const BLOCK_TOPIC: &str = "blocks";
pub const GOSSIP_CORE_TOPICS: [&str; 2] = [TRANSACTION_TOPIC, BLOCK_TOPIC];

pub fn type_of<T>(_: T) -> String {
    format!("{}", std::any::type_name::<T>()).to_string()
}


#[macro_export]
macro_rules! cast {
    ($x:expr, $t:path) => {
        match $x { 
            $t(value) => value, 
            _ => { panic!("cast to type {} failed ... actual {}", stringify!($t), type_of($x)) }
        }
    };
}


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
    mut producer_block_reciever: UnboundedReceiver<BlockHeader>,
    chain_state: ChainState, 
) -> Result<()> {
    let ChainState {
        fork_choice, 
        db, 
        p2p_keypair, 
        head_status,
        chain_keypair
    } = chain_state; 

    let local_peer_id = PeerId::from(p2p_keypair.public());

    let mut gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(p2p_keypair.clone()),
        gossipsub::Config::default(),
    )
    .unwrap();

    for topic in GOSSIP_CORE_TOPICS {
        let topic = Sha256Topic::new(topic);
        gossipsub.subscribe(&topic)?;
    }

    let mdns = mdns::async_io::Behaviour::new(mdns::Config::default(), local_peer_id)?;
    let behaviour = ChainBehaviour { gossipsub, mdns };

    // todo: use quicc
    let transport = libp2p::development_transport(p2p_keypair.clone()).await?;

    let mut swarm = Swarm::with_threadpool_executor(transport, behaviour, local_peer_id);
    let multi_addr = "/ip4/0.0.0.0/tcp/0".parse()?; 
    swarm.listen_on(multi_addr)?;

    let mut peer_manager = PeerManager::default();

    let metrics_tick_seconds = 4;
    let mut n_peers = 0;
    let mut txs_recieved = 0;
    let mut blocks_recieved = 0;
    let mut metrics_tick = interval(Duration::from_secs(metrics_tick_seconds));
    let _ = metrics_tick.tick().await; // scratch initial tick

    loop {
        select! {
            _ = metrics_tick.tick() => { 
                // note difficult to get this to work easily since we run multiple nodes on a single machine
                // datapoint_info!(
                //     "network", 
                //     ("n_peers", n_peers, i64), 
                //     ("txs_recieved", txs_recieved, i64), 
                //     ("blocks_recieved", blocks_recieved, i64), 
                // );
                
                let status = head_status.lock().await.clone();

                if status == HeadStatus::Behind { 
                    // check if we're at the head
                    let head_digest = fork_choice.lock().await.get_head().unwrap();
                    let mut n_matching_heads = 0;

                    let mut digests = Vec::with_capacity(peer_manager.len());
                    for (peer_id, client) in peer_manager.iter() {
                        let peer_head_digest = client.get_head(context::current()).await?.unwrap();
                        if peer_head_digest == head_digest { 
                            n_matching_heads += 1;
                        } else { 
                            // collect disagreement heads
                            digests.push((peer_head_digest, peer_id))
                        }
                    }
                    
                    // note assumption is once up to date is shouldnt be behind
                    let mut up_to_date_peers = n_peers / 2; 
                    if n_peers % 2 != 0 { 
                        up_to_date_peers += 1;
                    }
                    let is_up_to_date = n_matching_heads >= up_to_date_peers; 
                    info!("current head up to date: {n_matching_heads:?}/{n_peers:?}/{up_to_date_peers:?}...");

                    if is_up_to_date { 
                        info!("current head up to date: {n_matching_heads:?}/{n_peers:?}...");

                        // get the validator ID from the account
                        get_pinned!(db head_digest => head_block BlockHeader);
                        let state_root = head_block.state_root;
                        let account_digests = db.get_borsh::<AccountDigests>(state_root)?.0;
                        let index = account_digests.iter().position(|(_, addr)| *addr == VALIDATORS_ADDRESS).unwrap();
                        let (account_digest, _) = account_digests[index];

                        let validators = cast!(
                            db.get_borsh::<Account>(account_digest)?, 
                            Account::ValidatorsAccount
                        );
                        let index = validators.pubkeys.iter().position(|addr| *addr == chain_keypair.public.to_bytes());

                        if let Some(index) = index { 
                            let mut status = head_status.lock().await;
                            *status = HeadStatus::UpToDate(index as u32);
                            info!("status: {status:?}");
                        } else { 
                            info!("broadcasting add validator to list...");

                            // broadcast tx to add our pubkey to the network 
                            let tx = Transaction::AddValidatorTransaction(AddValidatorTransaction { 
                                pubkey: chain_keypair.public.to_bytes()
                            });
                            let tx = tx.sign(&chain_keypair);
    
                            let bytes = tx.try_to_vec()?;
                            let _ = swarm.behaviour_mut()
                                .gossipsub
                                .publish(Sha256Topic::new(TRANSACTION_TOPIC), bytes);

                            p2p_tx_sender.send(tx)?;
                        }
                    } else { 
                        // run block repair from some client 
                        if let Some((header_digest, peer_id)) = digests.get(0) {
                            let client = peer_manager.get_peer(peer_id).unwrap();
                            request_chain_from_block_hash(header_digest, client, &db, &fork_choice).await?;
                        }
                    }
                }
            },
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
                        n_peers += 1;
                    }
                },
                SwarmEvent::Behaviour(ChainBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                    for (peer_id, _multiaddr) in list {
                        swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                        peer_manager.remove_peer(&peer_id)?;
                        n_peers -= 1;
                    }
                },
                SwarmEvent::Behaviour(ChainBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                    message,
                    propagation_source, 
                    ..
                 })) => {
                    let data = message.data.as_slice();

                    if let Ok(tx) = SignedTransaction::try_from_slice(data) {
                        txs_recieved += 1;
                        // send to mempool
                        if tx.verify().is_ok() { 
                            p2p_tx_sender.send(tx)?; // clone :(
                        }
                        continue;
                    }

                    // todo: need to remove new block txs from the mempool
                    match bytemuck::try_from_bytes::<BlockHeader>(data) {
                        Ok(block_header) => {
                            blocks_recieved += 1;
                            let client = peer_manager.get_peer(&propagation_source).unwrap();
                            request_chain_from_header(&block_header, &client, &db, &fork_choice).await?;
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

macro_rules! verify {
    ($x:expr) => {
        if !$x { 
            return Err(anyhow!("block verification failed... {:?}", stringify!($x)));
        } else { 
            true
        }
    };
    ($x:expr, $y:expr) => {
        if !($x == $y) { 
            return Err(anyhow!("block verification failed... {:?} (${:?} != {:?})", 
                stringify!($x), 
                $x, $y
            ));
        } else { 
            true
        }
    };
}

pub async fn process_network_block(
    block: &Block, 
    db: Arc<RocksDB>, 
    fork_choice: Arc<Mutex<ForkChoice>>,
) -> Result<()> { 
    let parent_hash = block.header.parent_hash;
    get_pinned!(db parent_hash => parent_block_header BlockHeader);

    // multithread sigverify
    let txs = &block.txs.0;
    let result = txs.par_iter().find_any(|tx| tx.verify().is_err());
    if result.is_some() { 
        return Err(anyhow!("block tx sig verification failed..."));
    }
    let txs = Transactions(txs.clone());

    // re-produce the state change
    let (
        mut block_header,
        account_digests,
        new_accounts
    ) = state_transition(parent_block_header, &txs, db.clone())?;
    block_header.nonce = block.header.nonce;
    block_header.commit_block_hash();

    // verify final block
    info!("verifying block: {:?}", block_header);
    {
        verify!(block_header.block_hash, block.header.block_hash) &&
        verify!(block_header.state_root, block.header.state_root) &&
        verify!(block_header.tx_root, block.header.tx_root) &&
        verify!(block_header.is_valid_pow())
    };
    info!("block verification passed...");

    // store new block's state
    // todo: move behind forkchoice? 
    commit_new_block(block, account_digests, new_accounts, fork_choice.clone(), db.clone()).await?;

    Ok(())
}

pub async fn request_chain_from_block_hash(block_hash: &Sha256Bytes, client: &RPCClient, db: &Arc<RocksDB>, fork_choice: &Arc<Mutex<ForkChoice>>) -> anyhow::Result<()> {
    let block_header = client.get_block_header(context::current(), *block_hash).await?.unwrap();
    request_chain_from_header(&block_header, client, db, fork_choice).await
}

pub async fn request_chain_from_header(block_header: &BlockHeader, client: &RPCClient, db: &Arc<RocksDB>, fork_choice: &Arc<Mutex<ForkChoice>>) -> anyhow::Result<()> {
    let mut blocks_to_commit = vec![
        *block_header
    ];

    while !blocks_to_commit.is_empty() { 
        let header = blocks_to_commit.pop().unwrap();
        // request associated txs
        let txs = client.get_txs(context::current(), header.tx_root).await?.unwrap();
        let block = Block { header, txs };

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
                // todo: fix so we dont doss the first node on recovery
                info!("repairing missing parent block: {parent_hash:x?}...");
                // request missing parent block 
                let block = client.get_block_header(context::current(), parent_hash).await?.unwrap();
                blocks_to_commit.push(block);
            } else { 
                // if not missing parent then propogate it 
                return Err(e);
            }
        } 
    }
    Ok(())
}