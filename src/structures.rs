use std::vec;
use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::{Keypair, PublicKey, Signature, SignatureError};
use ed25519_dalek::{Signer, Verifier};
use sha2::{Digest, Sha256};

use anyhow::Result;

pub const HASH_BYTE_SIZE: usize = 32;
pub type Sha256Bytes = [u8; HASH_BYTE_SIZE];
pub type Key256Bytes = [u8; 32]; // public/private key
pub type SignatureBytes = [u8; Signature::BYTE_SIZE];

pub const TXS_PER_BLOCK: usize = 5;
pub const POW_N_ZEROS: usize = 3; // note: needs to be > 0
pub const POW_LEN_ZEROS: usize = POW_N_ZEROS - 1;

// defines the Account, Transaction, and Block structures of the blockchain

/* ACCOUNT STRUCTS */
#[derive(BorshDeserialize, BorshSerialize, Default, Debug, PartialEq, Eq, Clone)]
pub struct Account {
    pub address: Key256Bytes,
    pub amount: u128,
}

impl Account {
    pub fn digest(&self) -> Sha256Bytes {
        let bytes = self.try_to_vec().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

// [(digest, pubkey_bytes)]
#[derive(BorshDeserialize, BorshSerialize)]
pub struct AccountDigests(pub Vec<(Sha256Bytes, Key256Bytes)>);

impl AccountDigests {
    pub fn digest(&self) -> Sha256Bytes {
        let mut hasher = Sha256::new();
        self.0
            .iter()
            .for_each(|(d, _)| sha2::Digest::update(&mut hasher, d));
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

/* TRANSACTION STRUCTS */
#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone)]
pub struct Transaction {
    pub address: Key256Bytes,
    pub amount: u128,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq, Eq, Clone)]
pub struct SignedTransaction {
    pub transaction: Transaction,
    pub signature: Option<SignatureBytes>,
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
        SignedTransaction {
            transaction: self,
            signature: sig_bytes.try_into().unwrap(),
        }
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

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct Transactions(pub Vec<SignedTransaction>);

impl Transactions {
    pub fn digest(&self) -> Sha256Bytes {
        let mut hasher = Sha256::new();
        self.0
            .iter()
            .for_each(|d| sha2::Digest::update(&mut hasher, d.try_to_vec().unwrap().as_slice()));
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

/* BLOCK STRUCTS */
#[derive(BorshDeserialize, BorshSerialize, Debug, Default, Clone)]
pub struct BlockHeader {
    pub parent_hash: Sha256Bytes,
    pub state_root: Sha256Bytes,
    pub tx_root: Sha256Bytes,
    pub block_hash: Option<Sha256Bytes>,
    pub nonce: u128,
}

#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]
pub struct Block {
    pub header: BlockHeader,
    pub txs: Transactions,
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
        let mut block = BlockHeader {
            state_root: [0; HASH_BYTE_SIZE],
            tx_root: [0; HASH_BYTE_SIZE],
            parent_hash: [0; HASH_BYTE_SIZE],
            block_hash: None,
            nonce: 0,
        };
        block.commit_block_hash();
        block
    }

    pub fn commit_block_hash(&mut self) {
        self.block_hash = Some(self.compute_block_hash());
    }

    pub fn compute_block_hash(&self) -> Sha256Bytes {
        let mut hasher = Sha256::new();
        hasher.update(self.state_root);
        hasher.update(self.parent_hash);
        hasher.update(self.nonce.to_le_bytes());

        let bytes: Sha256Bytes = hasher.finalize().as_slice().try_into().unwrap();
        bytes
    }

    pub fn is_valid_pow(&self) -> bool {
        let hash = self.compute_block_hash();
        let n_zeros = POW_LEN_ZEROS.min(HASH_BYTE_SIZE);
        let mut pow_success = true;
        for value in hash.iter().take(n_zeros) {
            if *value != 0 {
                pow_success = false;
                break;
            }
        }
        pow_success
    }
}
