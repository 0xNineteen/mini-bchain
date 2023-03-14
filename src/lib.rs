pub mod network;
pub mod pow;
pub mod state;
pub mod structures;
pub mod fork_choice;

#[allow(clippy::all)]
#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use borsh::BorshSerialize;
    use rocksdb::DB;
    use std::vec;

    use structures::*;
    use fork_choice::ForkChoice;

    use borsh::BorshDeserialize;
    use ed25519_dalek::{Keypair, PublicKey, Signer, Verifier};
    use rand::rngs::OsRng;
    use sha2::{digest::Update, Digest, Sha256};

    #[test]
    fn fork_choice() {
        let block = BlockHeader::genesis();
        let mut fork_choice = ForkChoice::new(block.block_hash.unwrap());

        assert!(fork_choice.get_head().unwrap() == block.block_hash.unwrap());

        // new block
        let mut block_a = BlockHeader::default();
        block_a.parent_hash = block.block_hash.unwrap();
        block_a.commit_block_hash();

        fork_choice
            .insert(block_a.block_hash.unwrap(), block_a.parent_hash)
            .unwrap();

        assert!(fork_choice.get_head().unwrap() == block_a.block_hash.unwrap());

        // fork
        let mut block_b = BlockHeader {
            parent_hash: block.block_hash.unwrap(),
            ..BlockHeader::default()
        };
        block_b.commit_block_hash();

        fork_choice
            .insert(block_b.block_hash.unwrap(), block_b.parent_hash)
            .unwrap();

        // make sure it doesnt break on forks
        let _fork = fork_choice.get_head().unwrap();
    }

    #[test]
    fn block_transactions() {
        let txs = (0..3)
            .map(|i| {
                let keypair = example_keypair();
                Transaction {
                    address: keypair.public.to_bytes(),
                    amount: i,
                }
                .sign(&keypair)
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
        let account = Account {
            address: keypair.public.to_bytes(),
            amount: 10,
        };

        // add account to db
        let digest = account.digest();
        db.insert(digest, account.try_to_vec().unwrap());

        // compute new state
        let account_digests = vec![(digest, account.address)];
        let account_digests = AccountDigests(account_digests);

        // add state to db
        let state_root = account_digests.digest();
        db.insert(state_root, account_digests.try_to_vec().unwrap());

        // compute new block
        let mut block = BlockHeader {
            state_root,
            parent_hash: parent_block.block_hash.unwrap(),
            tx_root: [0; 32],
            block_hash: None,
            nonce: 0,
        };
        block.commit_block_hash();

        // store block in db
        db.insert(block.block_hash.unwrap(), block.try_to_vec().unwrap());
    }

    fn example_keypair() -> Keypair {
        let mut rng = OsRng {};

        Keypair::generate(&mut rng)
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
        let mut rng = OsRng {};
        let keypair = Keypair::generate(&mut rng);

        let msg = b"hello";
        let signature = keypair.sign(msg);
        let _bytes = signature.to_bytes();

        let publickey = keypair.public;
        let result = publickey.verify(msg, &signature);
        assert!(result.is_ok());

        let _account = Account {
            address: publickey.to_bytes(),
            amount: 0,
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

        db.put(key, value).unwrap();

        match db.get(key) {
            Ok(Some(v)) => {
                let _account = Account::try_from_slice(v.as_slice()).unwrap();
                assert_eq!(_account, account);
            }
            _ => {
                assert!(false)
            }
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
