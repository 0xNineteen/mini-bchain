use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::{vec, mem};
use ed25519_dalek::ed25519::signature::rand_core::block;
use rand::Rng;
use rocksdb::DB;
use sha2::digest::generic_array::functional::FunctionalSequence;
use sha2::{Sha256, Digest};
use borsh::{BorshSerialize, BorshDeserialize};
use ed25519_dalek::{Keypair, Signature, PublicKey, SignatureError};
use ed25519_dalek::{Signer, Verifier};
use rand::rngs::OsRng;
use anyhow::Result;
use anyhow::anyhow;

pub const HASH_BYTE_SIZE: usize = 32;
pub type Sha256Bytes = [u8; HASH_BYTE_SIZE];
pub type Key256Bytes = [u8; 32]; // public/private key
pub type SignatureBytes = [u8; Signature::BYTE_SIZE];

#[derive(BorshDeserialize, BorshSerialize, Default, Debug, PartialEq, Eq, Clone)]
pub struct Account { 
    pub address: Key256Bytes, 
    pub amount: u128
}

#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone)]
pub struct Transaction { 
    pub address: Key256Bytes, 
    pub amount: u128, 
}

#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone)]
pub struct SignedTransaction { 
    pub transaction: Transaction,
    pub signature: Option<SignatureBytes>
}

impl Transaction { 
    pub fn digest(&self) -> Sha256Bytes { 
        let bytes = self.try_to_vec().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().as_slice().try_into().unwrap()
    }

    // consumes 
    pub fn sign(self, keypair: &Keypair) -> SignedTransaction { 
        let digest = self.digest();
        let sig = keypair.sign(digest.as_slice());
        let sig_bytes = sig.to_bytes();
        SignedTransaction { transaction: self, signature: sig_bytes.try_into().unwrap() }
    }
}

impl SignedTransaction { 
    pub fn verify(&self) -> Result<(), SignatureError> { 
        let digest = self.transaction.digest();
        // todo: remove these unwrap()s and return a result<>
        let publickey = PublicKey::from_bytes(self.transaction.address.as_slice())?;
        let sig = Signature::from_bytes(self.signature.unwrap().as_slice())?;
        publickey.verify(digest.as_slice(), &sig)
    }
}

impl Account { 
    pub fn digest(&self) -> Sha256Bytes { 
        let bytes = self.try_to_vec().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Default, Clone)]
pub struct BlockHeader { 
    pub parent_hash: Sha256Bytes,
    pub state_root: Sha256Bytes, 
    pub tx_root: Sha256Bytes, 
    pub block_hash: Option<Sha256Bytes>,
    pub nonce: u128
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct Block { 
    pub header: BlockHeader, 
    pub txs: Transactions
}

impl Block { 
    pub fn genesis() -> Self { 
        let header = BlockHeader::genesis(); 
        let txs = Transactions(vec![]);
        Block { header, txs }
    }
}

impl BlockHeader { 
    pub fn genesis() -> Self { 
        let mut block = BlockHeader { state_root: [0; HASH_BYTE_SIZE], tx_root: [0; HASH_BYTE_SIZE], parent_hash: [0; HASH_BYTE_SIZE],  block_hash: None, nonce: 0};
        block.commit_block_hash();
        block
    }

    pub fn commit_block_hash(&mut self) { 
        self.block_hash = Some(self.compute_block_hash());
    }

    pub fn compute_block_hash(&mut self) -> Sha256Bytes { 
        let mut hasher = Sha256::new();
        hasher.update(self.state_root);
        hasher.update(self.parent_hash);
        hasher.update(self.nonce.to_le_bytes());

        let bytes: Sha256Bytes = hasher.finalize().as_slice().try_into().unwrap();
        bytes
    }
}

// [(digest, pubkey_bytes)]
#[derive(BorshDeserialize, BorshSerialize)]
pub struct AccountDigests(Vec<(Sha256Bytes, Key256Bytes)>);

impl AccountDigests { 
    pub fn digest(&self) -> Sha256Bytes { 
        let mut hasher = Sha256::new();
        self.0
            .iter()
            .for_each(|(d, _)| sha2::Digest::update(&mut hasher, d));
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct Transactions(Vec<SignedTransaction>);

impl Transactions { 
    pub fn digest(&self) -> Sha256Bytes { 
        let mut hasher = Sha256::new();
        self.0
            .iter()
            .for_each(| d | sha2::Digest::update(&mut hasher, d.try_to_vec().unwrap().as_slice()));
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

// client continuously sends txs 
// p2p collects transactions

use libp2p::futures::StreamExt;
use libp2p::gossipsub::{Sha256Topic};
use libp2p::{
    gossipsub, identity, mdns, swarm::NetworkBehaviour, swarm::SwarmEvent, PeerId, Swarm,
};
use tokio::runtime::Builder;
use tokio::select;
use tokio::sync::{RwLock, Mutex};
// use std::sync::Mutex;
use tokio::sync::mpsc::channel;
use tokio::time::{interval, sleep};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedSender, UnboundedReceiver, unbounded_channel};

use tracing_subscriber;
use tracing::{info, Instrument};

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
    Transaction(SignedTransaction), 
    Block(Block)
}

pub async fn network(
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
        gossipsub::Config::default()
    ).unwrap();

    for topic in GOSSIP_CORE_TOPICS { 
        let topic = Sha256Topic::new(topic);
        gossipsub.subscribe(&topic)?;
    }

    let mdns = mdns::async_io::Behaviour::new(mdns::Config::default(), local_peer_id)?;
    let behaviour = ChainBehaviour { 
        gossipsub, 
        mdns
    };

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
                info!("publishing tx..");

                let mut rng = OsRng{};
                let keypair = Keypair::generate(&mut rng);
                let transaction = Transaction { 
                    address: keypair.public.to_bytes(), 
                    amount: 420
                };
                let transaction = transaction.sign(&keypair);
                let transaction = Broadcast::Transaction(transaction);
                let bytes = transaction.try_to_vec()?;

                let result = swarm.behaviour_mut()
                    .gossipsub
                    .publish(Sha256Topic::new(TRANSACTION_TOPIC), bytes);

                if let Err(e) = result { 
                    info!("tx publish err: {e:?}");
                }
            }
            Some(block) = producer_block_reciever.recv() => { 
                info!("publishing block...");

                let bytes = Broadcast::Block(block).try_to_vec()?;
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
                    let event = Broadcast::try_from_slice(message.data.as_slice())?;
                    match event { 
                        Broadcast::Transaction(tx) => {
                            info!("new p2p tx...");
                            // send to mempool
                            p2p_tx_sender.send(tx)?;
                        },
                        Broadcast::Block(block) => {
                            info!("new p2p block ...");
                            let block_hash = block.header.block_hash.unwrap(); 
                            let parent_hash = block.header.parent_hash; 
            
                            // TODO: do some validation here 

                            // re-produce the state change 
                            // store new block's state
            
                            // TODO: handle when parent hash not found (ie, request parent from reciever)
                            fork_choice.lock().await.insert(block_hash, parent_hash).unwrap();
                            db.as_ref().put(block_hash, block.try_to_vec()?)?;
                        }, 
                    } 
                }
                _ => { }
            }
        }
    }
}

const TXS_PER_BLOCK: usize = 1;
const POW_N_ZEROS: usize = 3; // note: needs to be > 0
const POW_LEN_ZEROS: usize = POW_N_ZEROS - 1;

pub async fn block_producer(
    mut p2p_tx_reciever: UnboundedReceiver<SignedTransaction>,
    p2p_block_sender: UnboundedSender<Block>,
    fork_choice: Arc<Mutex<ForkChoice>>, 
    db: Arc<DB>,
) -> Result<()> { 
    let mut mempool = vec![];
    let mut current_head = fork_choice.lock().await.get_head().unwrap();
    
    loop { 
        // add new txs to memepool
        while let Ok(tx) = p2p_tx_reciever.try_recv() {
            // do some verification here 
            info!("new tx...");
            if tx.verify().is_ok() { 
                info!("tx verification passed!");
                mempool.push(tx);
            } else { 
                info!("tx verification failed...");
            }
        } 

        if mempool.len() < TXS_PER_BLOCK { 
            info!("not enought txs ({} < {TXS_PER_BLOCK:?}), sleeping...", mempool.len());
            sleep(Duration::from_secs(3)).await;
            continue;
        }
        info!("POW mempool length: {}", mempool.len());

        info!("producing new block...");

        // sample N txs from mempool
        let txs = &mempool[..TXS_PER_BLOCK];

        // compute state transitions 
        // TODO: change to get pinned to reduce memory copy of reading values
        let head_block = Block::try_from_slice(
            db.get(current_head)?.unwrap().as_slice()
        )?;
        let state_root = head_block.header.state_root;
        let mut account_digests = AccountDigests::try_from_slice(
            db.get(state_root)?.unwrap().as_slice()
        )?.0;

        // build new block 
        info!("building new block state...");
        let mut local_db = HashMap::new();

        for tx in txs { 
            let tx = &tx.transaction; 
            let pubkey = tx.address;
            let result = account_digests.iter().position(|(_, addr)| *addr == pubkey);
            
            let new_account = match result { 
                Some(index) => { 
                    let (account_digest, _) = account_digests[index];
                    // look up existing account
                    let mut account = Account::try_from_slice(
                        db.get(account_digest)?.unwrap().as_slice()
                    )?;
                    assert!(account.address == pubkey);
                    // process the tx 
                    account.amount = tx.amount;
                    // update digest list
                    account_digests.remove(index);
                    account
                }, 
                None => { 
                    let account = Account { 
                        address: pubkey, 
                        amount: tx.amount
                    };
                    account
                }
            };

            let digest = new_account.digest();
            let account_bytes = new_account.try_to_vec()?;

            local_db.insert(digest, account_bytes);
            account_digests.push((digest, pubkey));
        }

        let txs = txs.to_vec();
        let txs = Transactions(txs);
        let tx_root = txs.digest();

        let account_digests = AccountDigests(account_digests);
        let state_root = account_digests.digest();

        let mut block_header = BlockHeader { 
            parent_hash: head_block.header.block_hash.unwrap(), 
            state_root, 
            tx_root, 
            block_hash: None, 
            nonce: 1
        };

        pub fn pow_loop(block_header: &mut BlockHeader, n_loops: usize) -> bool { 
            for _ in 0..n_loops { 
                let hash = block_header.compute_block_hash();
                let n_zeros = POW_LEN_ZEROS.min(HASH_BYTE_SIZE);
                let mut pow_success = true;
                for i in 0..n_zeros { 
                    if hash[i] != 0 { 
                        pow_success = false;
                        break;
                    }
                }
                if pow_success { 
                    return true;
                }

                // increment nonce 
                block_header.nonce += 1;
            }
            false
        }
        
        info!("running POW loop...");
        // if success => { insert in DB + send to p2p to broadcast } 
        // else new_head => { reset }
        loop { 
            // do pow for a few rounds
            let result = pow_loop(&mut block_header, 10);
            if result { 
                // remove blocked txs from mempool 
                for i in 0..TXS_PER_BLOCK { 
                    mempool.remove(i);
                }

                // insert local state
                // insert block into db 
                block_header.commit_block_hash();
                info!("new POW block produced: {:x?}", block_header.block_hash);

                let block = Block { 
                    header: block_header, 
                    txs
                };
                // * block_digest => block
                db.put(block.header.block_hash.unwrap(), block.try_to_vec()?)?;

                // * block.state_root => vec[digest]
                db.put(state_root, account_digests.try_to_vec()?)?;

                // * new accounts: digest => account
                for (k, v) in local_db { 
                    db.put(k, v)?;
                }

                // send block to p2p + block manager
                p2p_block_sender.send(block.clone())?;

                let block_hash = block.header.block_hash.unwrap(); 
                let parent_hash = block.header.parent_hash; 
                fork_choice.lock().await.insert(block_hash, parent_hash).unwrap();
                db.as_ref().put(block_hash, block.try_to_vec()?)?;

                let head = fork_choice.lock().await.get_head().unwrap();
                assert!(head != current_head);
                break; // need this break to satisify borrow checker
            }

            // check for new head (from p2p) everyonce in a while
            let head = fork_choice.lock().await.get_head().unwrap();
            if head != current_head {
                info!("[break] new chain head: {:?} -> {:?}", current_head, head);
                current_head = head;
                break; 
            }
        }
    }
}

use std::collections::BinaryHeap; 

pub struct ForkChoice {  
    block_heights: HashMap<Sha256Bytes, u32>, 
    // sorted so we know it will be consistent across nodes
    heads: BinaryHeap<Sha256Bytes>, 
    head_height: u32
}

impl ForkChoice { 
    pub fn new(block_hash: Sha256Bytes) -> Self { 
        let mut block_heights = HashMap::new(); 
        let mut heads = BinaryHeap::new();
        let head_height = 0;

        block_heights.insert(block_hash, 0);
        heads.push(block_hash);

        ForkChoice { block_heights, heads, head_height }
    }

    pub fn insert(&mut self, block_hash: Sha256Bytes, parent_hash: Sha256Bytes) -> Result<()> { 
        let parent_height = self.block_heights.get(&parent_hash);
        if parent_height.is_none() { 
            return Err(anyhow!("parent block hash DNE in fork choice"));
        }
        let parent_height = parent_height.unwrap();
        let block_height = parent_height + 1; 

        if block_height > self.head_height { 
            // new head
            self.heads.clear(); 
            self.head_height = block_height;
            self.heads.push(block_hash);

        } else if block_height == self.head_height { 
            // another tie
            self.heads.push(block_hash);

        }
        self.block_heights.insert(block_hash, block_height);

        Ok(())
    }

    pub fn get_head(&self) -> Option<Sha256Bytes> { 
        self.heads.peek().cloned()
    }
}

pub fn main() -> Result<()> { 
    tracing_subscriber::fmt::init();
    let runtime = Builder::new_multi_thread().enable_all().build().unwrap();

    // init db 
    let id = rand::thread_rng().gen_range(0, 100);
    info!("using id: {id}");
    let path = format!("../db_{id}/");

    let db = DB::open_default(path).unwrap();
    // we need shared access to the db 
    let db = Arc::new(db);

    runtime.block_on(async move { 
        let (p2p_tx_sender, p2p_tx_reciever) = unbounded_channel();
        let (producer_block_sender, producer_block_reciever) = unbounded_channel(); // producer => p2p

        // init genesis
        let mut genesis = Block::genesis();
        let genesis_hash = genesis.header.block_hash.unwrap();
        info!("genisis hash: {:x?}", genesis_hash);

        let account_digests = AccountDigests(vec![]);
        let state_root = account_digests.digest();
        genesis.header.state_root = state_root;

        db.as_ref().put(state_root, account_digests.try_to_vec().unwrap()).unwrap();
        db.as_ref().put(genesis_hash, genesis.try_to_vec().unwrap()).unwrap();

        // setup fork choice with genesis
        let fork_choice = ForkChoice::new(genesis_hash);
        let fork_choice = Arc::new(Mutex::new(fork_choice));

        // begin
        let fc_ = fork_choice.clone();
        let db_ = db.clone();
        tokio::spawn( async move { 
            block_producer(
                p2p_tx_reciever,
                producer_block_sender, 
                fc_, 
                db_
            )
            .instrument(tracing::info_span!("block producer"))
            .await.unwrap()
        });

        tokio::spawn(async move { 
            network(
                p2p_tx_sender, 
                producer_block_reciever,
                fork_choice, 
                db
            )
            .instrument(tracing::info_span!("network"))
            .await.unwrap()
        });

        loop { }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use ed25519_dalek::{Signer, Verifier};
    use sha2::digest::Update;

    #[test]
    fn fork_choice() { 
        let block = BlockHeader::genesis(); 
        let mut fork_choice = ForkChoice::new(block.block_hash.unwrap()); 

        assert!(fork_choice.get_head().unwrap() == block.block_hash.unwrap());

        // new block 
        let mut block_a = BlockHeader::default(); 
        block_a.parent_hash = block.block_hash.unwrap();
        block_a.commit_block_hash();

        fork_choice.insert(
            block_a.block_hash.unwrap(), 
            block_a.parent_hash
        ).unwrap();

        assert!(fork_choice.get_head().unwrap() == block_a.block_hash.unwrap());

        // fork 
        let mut block_b = BlockHeader::default(); 
        block_b.parent_hash = block.block_hash.unwrap();
        block_b.commit_block_hash();

        fork_choice.insert(
            block_b.block_hash.unwrap(), 
            block_b.parent_hash
        ).unwrap();

        // make sure it doesnt break on forks
        let fork = fork_choice.get_head().unwrap();
    }

    #[test]
    fn block_transactions() { 
        let txs = (0..3).map(|i| { 
            let keypair = example_keypair();
            Transaction { 
                address: keypair.public.to_bytes(), 
                amount: i
            }.sign(&keypair)
        })
        .collect::<Vec<_>>();
        let txs = Transactions(txs);
        // compute root
        let tx_digest = txs.digest();

        // pow block searching
        let mut block = BlockHeader::genesis(); 
        block.tx_root = tx_digest;

        // u8 = 8 bits = 256
        while block.compute_block_hash()[0] > 0 { 
            block.nonce += 1;
        }
        block.commit_block_hash();

        println!("{:?}", block.block_hash);

    }

    #[test]
    fn block_state() { 
        // defn: 
        // state = AccountDigests 
        // account = Account 
        // db = DB 

        // db for tests
        let mut db = HashMap::new();

        // --- 
        // init chain
        let genesis = BlockHeader::genesis(); 
        db.insert(genesis.block_hash.unwrap(), genesis.try_to_vec().unwrap());

        let parent_block = genesis; // todo: fork choice struct

        // --- 
        // init new account
        let keypair = example_keypair();
        let account = Account { address: keypair.public.to_bytes(), amount: 10 };

        // add account to db
        let digest = account.digest();
        db.insert(digest, account.try_to_vec().unwrap());

        // compute new state
        let mut account_digests = vec![];
        account_digests.push((digest.clone(), account.address));
        let account_digests = AccountDigests(account_digests);

        // add state to db
        let state_root = account_digests.digest();
        db.insert(state_root, account_digests.try_to_vec().unwrap());

        // compute new block 
        let mut block = BlockHeader { state_root, parent_hash: parent_block.block_hash.unwrap(), tx_root: [0; 32], block_hash: None, nonce: 0 }; 
        block.commit_block_hash();

        // store block in db
        db.insert(block.block_hash.unwrap(), block.try_to_vec().unwrap());

    }

    fn example_keypair() -> Keypair { 
        let mut rng = OsRng{};
        let keypair = Keypair::generate(&mut rng);
        keypair
    }

    #[test]
    fn transaction_test() { 
        let keypair = example_keypair();
        let publickey = keypair.public;

        let transaction = Transaction { 
            address: publickey.to_bytes(), 
            amount: 1, 
        };
        let transaction = transaction.sign(&keypair);

        assert!(transaction.verify().is_ok())
    }

    #[test]
    fn keypair_test() { 
        let mut rng = OsRng{};
        let keypair = Keypair::generate(&mut rng);

        let msg = b"hello";
        let signature = keypair.sign(msg);
        let _bytes = signature.to_bytes();

        let publickey = keypair.public; 
        let result = publickey.verify(msg, &signature);
        assert!(result.is_ok());

        let _account = Account { 
            address: publickey.to_bytes(), 
            amount: 0
        };
        let _publickey = PublicKey::from_bytes(&_account.address).unwrap();
        assert_eq!(_publickey, publickey);
    }
    
    #[test] 
    fn db_insert() { 
        let path = "../db/";
        let db = DB::open_default(path).unwrap();

        let account = Account::default(); 
        let value = account.try_to_vec().unwrap();
        let key = account.digest();

        db.put(key.clone(), value).unwrap();

        match db.get(key) { 
            Ok(Some(v)) => {
                let _account = Account::try_from_slice(v.as_slice()).unwrap();
                assert_eq!(_account, account);
            }, 
            _ => { assert!(false) }
        }
    }

    #[test] 
    fn account_serialization() { 
        let hasher = Sha256::new();
        let result = hasher.chain("some address").finalize().to_vec();
        let result = &result[..32];
        
        let account = Account {
            address: result.try_into().unwrap(),
            amount: 1000,
        };

        let bytes = account.try_to_vec().unwrap();
        let _account = Account::try_from_slice(bytes.as_slice()).unwrap();

        assert_eq!(_account, account);
    }

    #[test] 
    fn hash_test() { 
       let mut hasher = Sha256::new();
       sha2::Digest::update(&mut hasher, b"hi");
       let result = hasher.finalize();
       let result = result.to_vec();
       println!("{result:?}");
    }

}