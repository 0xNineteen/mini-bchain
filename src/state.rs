
use std::sync::Arc;
use rocksdb::DB;
use borsh::{BorshDeserialize, BorshSerialize};
use anyhow::{Result};
use tokio::sync::Mutex;
use tracing::info;

use crate::structures::*;
use crate::fork_choice::ForkChoice;

pub struct RocksDB { 
    pub db: DB
}

pub trait ChainDB { 
    fn get<T: BorshDeserialize>(&self, key: Sha256Bytes) -> Result<T>;
    fn put<T: BorshSerialize + ChainDigest>(&self, value: &T) -> Result<Sha256Bytes>;
}

impl ChainDB for RocksDB { 
    fn get<T: BorshDeserialize>(&self, key: Sha256Bytes) -> Result<T> {
        Ok(T::try_from_slice(
            self.db.get(key)?.unwrap().as_slice()
        )?)
    }

    fn put<T: BorshSerialize + ChainDigest>(&self, value: &T) -> Result<Sha256Bytes> {
        let key = value.digest();
        self.db.put(key, value.try_to_vec()?)?;
        Ok(key)
    }
}

pub fn state_transition<DB: ChainDB>(
    parent_block: Block,
    txs: &[SignedTransaction],
    db: Arc<DB>,
) -> Result<(BlockHeader, AccountDigests, Vec<Account>)> {
    let state_root = parent_block.header.state_root;
    let mut account_digests = db.get::<AccountDigests>(state_root)?.0;

    // build new block
    info!("building new block state...");
    let mut new_accounts = vec![];

    for tx in txs {
        let tx = &tx.transaction;
        let pubkey = tx.address;
        let result = account_digests.iter().position(|(_, addr)| *addr == pubkey);

        let new_account = match result {
            Some(index) => {
                let (account_digest, _) = account_digests[index];
                // look up existing account
                let mut account = db.get::<Account>(account_digest)?;
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

        new_accounts.push(new_account);
        account_digests.push((digest, pubkey));
    }

    let txs = txs.to_vec();
    let txs = Transactions(txs);
    let tx_root = txs.digest();

    let account_digests = AccountDigests(account_digests);
    let state_root = account_digests.digest();

    let block_header = BlockHeader {
        parent_hash: parent_block.header.block_hash.unwrap(),
        state_root,
        tx_root,
        block_hash: None,
        nonce: 1,
    };

    Ok((block_header, account_digests, new_accounts))
}

pub async fn commit_new_block<T: ChainDB>(
    block: &Block,
    account_digests: AccountDigests,
    new_accounts: Vec<Account>,
    fork_choice: Arc<Mutex<ForkChoice>>,
    db: Arc<T>,
) -> Result<()> {

    for account in new_accounts {
        db.put(&account)?;
    }
    db.put(&account_digests)?;
    db.put(block)?;

    // TODO: handle when parent hash not found (ie, request parent from reciever)
    let block_hash = block.header.block_hash.unwrap();
    let parent_hash = block.header.parent_hash;

    fork_choice
        .lock()
        .await
        .insert(block_hash, parent_hash)
        .unwrap();

    Ok(())
}
