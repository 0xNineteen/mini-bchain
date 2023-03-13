use std::sync::Arc;
use std::vec;
use rocksdb::DB;
use borsh::BorshDeserialize;
use anyhow::Result;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::sleep;
use tracing::info;

use crate::structures::*;
use crate::fork_choice::*;
use crate::state::*;

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
            info!(
                "not enought txs ({} < {TXS_PER_BLOCK:?}), sleeping...",
                mempool.len()
            );
            sleep(Duration::from_secs(3)).await;
            continue;
        }
        info!("POW mempool length: {}", mempool.len());

        info!("producing new block...");

        // sample N txs from mempool
        let txs = &mempool[..TXS_PER_BLOCK];

        // compute state transitions

        // TODO: change to get pinned to reduce memory copy of reading values
        let head_block = Block::try_from_slice(db.get(current_head)?.unwrap().as_slice())?;
        let (mut block_header, account_digests, local_db) =
            state_transition(head_block, txs, db.clone())?;

        pub fn pow_loop(block_header: &mut BlockHeader, n_loops: usize) -> bool {
            for _ in 0..n_loops {
                if block_header.is_valid_pow() {
                    block_header.commit_block_hash(); // save it
                    return true;
                }
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
                info!("new POW block produced: {:x?}", block_header.block_hash);

                // remove blocked txs from mempool
                let txs = Transactions(txs.to_vec());
                for _ in 0..TXS_PER_BLOCK {
                    mempool.remove(0);
                }

                let block = Block {
                    header: block_header,
                    txs,
                };
                p2p_block_sender.send(block.clone())?;

                // update state
                commit_new_block(
                    &block,
                    account_digests,
                    local_db,
                    fork_choice.clone(),
                    db.clone(),
                )
                .await?;

                // update head
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