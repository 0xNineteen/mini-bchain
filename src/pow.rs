use std::ops::Deref;

use std::vec;
use anyhow::Result;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::sleep;
use tracing::{info, debug};

use crate::structures::*;

use crate::machine::*;

use crate::get_pinned;

// todo: include a state s.t if the network fails this auto-stops
pub async fn block_producer(
    mut p2p_tx_reciever: UnboundedReceiver<SignedTransaction>,
    p2p_block_sender: UnboundedSender<BlockHeader>,
    chain_state: ChainState, 
) -> Result<()> {
    let ChainState {
        fork_choice, 
        db, 
        ..
    } = chain_state; 

    let mut mempool = vec![];
    let mut current_head = fork_choice.lock().await.get_head().unwrap();

    loop {
        // add new txs to memepool
        while let Ok(tx) = p2p_tx_reciever.try_recv() {
            // do some verification here
            // info!("new tx...");
            if tx.verify().is_ok() && !mempool.contains(&tx) {
                // info!("tx verification passed!");
                mempool.push(tx);
            } else {
                // info!("tx verification failed...");
            }
        }

        if mempool.len() == 0 {
            debug!(
                "not enought txs ({} < {TXS_PER_BLOCK:?}), sleeping...",
                mempool.len()
            );
            sleep(Duration::from_secs(3)).await;
            continue;
        }
        info!("POW mempool length: {}", mempool.len());

        info!("producing new block...");

        // sample N txs from mempool
        let n_txs = TXS_PER_BLOCK.min(mempool.len()); 
        let txs = Transactions(mempool[..n_txs].to_vec());
        get_pinned!(db current_head => head_block_header BlockHeader);

        // build potential block
        let (mut block_header, account_digests, new_accounts) =
            state_transition(head_block_header, &txs, db.clone())?;

        info!("running POW loop...");
        // if success => { insert in DB + send to p2p to broadcast }
        // else new_head => { reset }
        loop {
            // do pow for a few rounds
            let result = pow_loop(&mut block_header, 10);
            if result {
                info!("new POW block produced: {:x?}", block_header.block_hash);

                // remove blocked txs from mempool
                for _ in 0..n_txs {
                    mempool.remove(0);
                }

                p2p_block_sender.send(block_header.clone())?;

                let block = Block {
                    header: block_header,
                    txs,
                };
                // update state
                commit_new_block(
                    &block,
                    account_digests,
                    new_accounts,
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
                info!("[break] new chain head");
                current_head = head;
                break;
            }
        }
    }
}

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