use std::collections::HashMap;
use std::sync::Arc;
use anyhow::{Result};

use ed25519_dalek::Keypair as edKeypair;
use libp2p::identity::Keypair;
use tokio::sync::Mutex;
use tracing::info;

use crate::structures::*;
use crate::fork_choice::ForkChoice;
use crate::db::*;

#[derive(Default, Debug, PartialEq, Eq, Clone)]
pub enum HeadStatus { 
    UpToDate(u32), 
    #[default]
    Behind
}

#[derive(Clone)]
pub struct ChainState { 
    pub fork_choice: Arc<Mutex<ForkChoice>>, 
    pub db: Arc<RocksDB>, 
    pub p2p_keypair: Keypair,
    pub chain_keypair: Arc<edKeypair>,
    pub head_status: Arc<Mutex<HeadStatus>>,
}

// todo: use hash tree lookup (eth full optimized)
pub fn state_transition(
    parent_block_header: &BlockHeader,
    txs: &Transactions,
    db: Arc<RocksDB>,
) -> Result<(BlockHeader, AccountDigests, Vec<Account>)> {
    let state_root = parent_block_header.state_root;
    let mut account_digests = db.get_borsh::<AccountDigests>(state_root)?.0;

    // build new block
    info!("building new block state with {:?} txs...", txs.0.len());
    let tx_root = txs.digest();
    let mut new_accounts = Vec::with_capacity(txs.0.len());

    // todo: parallel processing txs
    let mut new_digest_indexs: HashMap<Sha256Bytes, (Account, usize)> = HashMap::new();

    for signed_tx in &txs.0 {
        let tx = &signed_tx.transaction;
        let tx_address = tx.get_address()?;

        // tx modifies account to new digest 
        // another account wants to modify that new account => digest doesnt exist in the database yet ... 

        let result = account_digests.iter().position(|(_, addr)| *addr == tx_address);
        let account = match result {
            Some(index) => {
                let (account_digest, _) = account_digests[index];

                // remove old reference
                account_digests.remove(index);

                // look up existing account
                if let Ok(account) = db.get_borsh::<Account>(account_digest) { 
                    Some(account)
                } else { 
                    if let Some((account, index)) = new_digest_indexs.get(&account_digest) { 
                        new_accounts.remove(*index);
                        Some(account.clone())
                    } else { 
                        panic!("account and/or digest not found")
                    }
                }
            }
            None => None,
        };

        let new_account = tx.process(account)?;
        let digest = new_account.digest();

        new_digest_indexs.insert(digest, (new_account.clone(), new_accounts.len()));
        new_accounts.push(new_account);
        account_digests.push((digest, tx_address));
    }

    let account_digests = AccountDigests(account_digests);
    let state_root = account_digests.digest();

    let block_header = BlockHeader {
        parent_hash: parent_block_header.block_hash,
        state_root,
        tx_root,
        height: parent_block_header.height + 1,
        .. BlockHeader::default()
    };

    Ok((block_header, account_digests, new_accounts))
}

pub async fn commit_new_block(
    block: &Block,
    account_digests: AccountDigests,
    new_accounts: Vec<Account>,
    fork_choice: Arc<Mutex<ForkChoice>>,
    db: Arc<RocksDB>,
) -> Result<()> {

    for account in new_accounts {
        db.put_borsh(&account)?;
    }
    db.put_borsh(&account_digests)?;
    
    // store header + txs seperately
    db.put_pod(&block.header)?;
    db.put_borsh(&block.txs)?;

    let block_hash = block.header.block_hash;
    let parent_hash = block.header.parent_hash;

    fork_choice
        .lock()
        .await
        .insert(block_hash, parent_hash)?;

    Ok(())
}

#[cfg(test)] 
mod tests { 
    use std::sync::Arc;

    use ed25519_dalek::Keypair;
    use rand::rngs::OsRng;
    use rocksdb::DB;

    use crate::{get_tmp_ledger_path_auto_delete, get_pinned};

    use super::*;

    #[test]
    pub fn test_machine() -> Result<()> { 
        let path = get_tmp_ledger_path_auto_delete!();
        let db = DB::open_default(path).unwrap();
        let db = RocksDB { db };
        let db = Arc::new(db);

        let fc = db.insert_genesis().unwrap();
        let genesis_hash = fc.get_head().unwrap();
        let fc = Arc::new(Mutex::new(fc));

        // random tx
        let mut rng = OsRng{};
        let keypair = Keypair::generate(&mut rng);
        let transaction = Transaction::AccountTransaction(AccountTransaction {
            pubkey: keypair.public.to_bytes(),
            amount: 420
        });
        let transaction = transaction.sign(&keypair);

        let mut txs = Transactions::default().0;
        txs.push(transaction);
        let txs = Transactions(txs);

        get_pinned!(db genesis_hash => parent_block_header BlockHeader);

        let (
            block_header,
            account_digests,
            new_accounts
        ) = state_transition(parent_block_header, &txs, db.clone())?;

        assert_eq!(block_header.parent_hash, parent_block_header.block_hash);
        assert_eq!(block_header.state_root, account_digests.digest());
        assert_eq!(new_accounts.len(), 1);
        assert_eq!(account_digests.0.len(), 2);

        // correct transition
        let account = &new_accounts[0];
        if let Account::UserAccount(account) = account { 
            assert_eq!(account.address, keypair.public.to_bytes());
            assert_eq!(account.amount, 420);
        } else { 
            assert!(false);
        }

        let block = Block { header: block_header, txs };

        tokio_test::block_on(
            commit_new_block(&block, account_digests, new_accounts, fc.clone(), db.clone())
        )?;

        // new head
        let new_head = fc.blocking_lock().get_head();
        assert_eq!(new_head.unwrap(), block.header.block_hash);

        // digests in state 
        let digests: AccountDigests = db.get_borsh(block_header.state_root)?;
        let (account_digest, address) = digests.0[1];
        assert_eq!(address, keypair.public.to_bytes());

        // correct transition is stored correctly
        let account: Account = db.get_borsh(account_digest)?;
        if let Account::UserAccount(account) = account { 
            assert_eq!(account.address, address);
            assert_eq!(account.amount, 420);
        } else { 
            assert!(false);
        }

        Ok(())
    }

}