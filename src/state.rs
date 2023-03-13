use std::collections::HashMap;
use std::sync::Arc;
use rocksdb::DB;
use borsh::{BorshDeserialize, BorshSerialize};
use anyhow::Result;
use tokio::sync::Mutex;
use tracing::info;

use crate::structures::*;
use crate::fork_choice::ForkChoice;

pub fn state_transition(
    head_block: Block,
    txs: &[SignedTransaction],
    db: Arc<DB>,
) -> Result<(BlockHeader, AccountDigests, HashMap<Sha256Bytes, Vec<u8>>)> {
    let state_root = head_block.header.state_root;
    let mut account_digests =
        AccountDigests::try_from_slice(db.get(state_root)?.unwrap().as_slice())?.0;

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
                let mut account =
                    Account::try_from_slice(db.get(account_digest)?.unwrap().as_slice())?;
                assert!(account.address == pubkey);
                // process the tx
                account.amount = tx.amount;
                // update digest list
                account_digests.remove(index);
                account
            }
            None => Account {
                address: pubkey,
                amount: tx.amount,
            },
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

    let block_header = BlockHeader {
        parent_hash: head_block.header.block_hash.unwrap(),
        state_root,
        tx_root,
        block_hash: None,
        nonce: 1,
    };

    Ok((block_header, account_digests, local_db))
}

pub async fn commit_new_block(
    block: &Block,
    account_digests: AccountDigests,
    local_db: HashMap<Sha256Bytes, Vec<u8>>,
    fork_choice: Arc<Mutex<ForkChoice>>,
    db: Arc<DB>,
) -> Result<()> {
    // * block_digest => block
    db.put(block.header.block_hash.unwrap(), block.try_to_vec()?)?;

    // * block.state_root => vec[digest]
    db.put(block.header.state_root, account_digests.try_to_vec()?)?;

    // * new accounts: digest => account
    for (k, v) in local_db {
        db.put(k, v)?;
    }

    // TODO: handle when parent hash not found (ie, request parent from reciever)
    let block_hash = block.header.block_hash.unwrap();
    let parent_hash = block.header.parent_hash;

    fork_choice
        .lock()
        .await
        .insert(block_hash, parent_hash)
        .unwrap();
    db.as_ref().put(block_hash, block.try_to_vec()?)?;

    Ok(())
}
