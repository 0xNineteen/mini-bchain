use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::{Keypair, PublicKey, Signature, SignatureError};
use ed25519_dalek::{Signer, Verifier};
use sha2::{Digest, Sha256};

use anyhow::Result;

use bytemuck::{Pod, Zeroable, bytes_of};
use serde::{Serialize, Deserialize, Deserializer}; 
use serde_bytes;

pub const HASH_BYTE_SIZE: usize = 32;
pub type Sha256Bytes = [u8; HASH_BYTE_SIZE];
pub type Key256Bytes = [u8; 32]; // public/private key

#[repr(C)]
#[derive(Pod, Zeroable, Copy, Debug, PartialEq, Eq, Clone)]
pub struct SignatureBytes([u8; Signature::BYTE_SIZE]);

impl Serialize for SignatureBytes { 
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
        where
            S: serde::Serializer {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for SignatureBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where D: Deserializer<'de>
    {
        let bytes: Vec<u8> = serde_bytes::deserialize(deserializer)?;
        let mut array = [0u8; Signature::BYTE_SIZE];
        array.copy_from_slice(&bytes);
        Ok(SignatureBytes(array))
    }
}


// pub const TXS_PER_BLOCK: usize = 2048; // todo: make this dynamic ? 
pub const TXS_PER_BLOCK: usize = 2; // todo: make this dynamic ? 
pub const POW_N_ZEROS: usize = 3; // note: needs to be > 0
pub const POW_LEN_ZEROS: usize = POW_N_ZEROS - 1;


// defines the Account, Transaction, and Block structures of the blockchain

pub trait ChainDigest { 
    // todo: use a proc macro to derive
    fn digest(&self) -> Sha256Bytes;
}

/* ACCOUNT STRUCTS */
#[repr(C)]
#[derive(Pod, Zeroable, Copy, Default, Debug, PartialEq, Eq, Clone)]
pub struct Account {
    pub address: Key256Bytes,
    pub amount: u128,
}

impl ChainDigest for Account {
    fn digest(&self) -> Sha256Bytes {
        let bytes = bytemuck::bytes_of(self);
        let mut hasher = Sha256::new();
        hasher.update(bytes); // copy
        hasher.finalize().as_slice().try_into().unwrap()
    }
}


// this contains all the validators on the network
#[repr(C)]
#[derive(BorshDeserialize, BorshSerialize, Default, Debug, PartialEq, Eq, Clone)]
pub struct ValidatorsAccount {
    pub pubkeys: Vec<Key256Bytes>,
}

impl ChainDigest for ValidatorsAccount {
    fn digest(&self) -> Sha256Bytes {
        let bytes = self.try_to_vec().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes); // copy
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

// [(digest, pubkey_bytes)]
// cant impl Copy bc its a Vec and so cant impl Pod/zero-copy :(
#[derive(BorshDeserialize, BorshSerialize, Clone)] 
pub struct AccountDigests(pub Vec<(Sha256Bytes, Key256Bytes)>);

impl ChainDigest for AccountDigests {
    fn digest(&self) -> Sha256Bytes {
        let bytes = self.try_to_vec().unwrap();
        let mut hasher = Sha256::new();
        hasher.update(bytes); // copy
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

/* TRANSACTION STRUCTS */
#[repr(C)]
#[derive(Pod, Zeroable, Copy, Debug, PartialEq, Eq, Clone, Default, Serialize, Deserialize)]
pub struct Transaction {
    pub address: Key256Bytes,
    pub amount: u128,
}

#[repr(C)]
#[derive(Pod, Zeroable, Copy, Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct SignedTransaction {
    pub transaction: Transaction,
    pub signature: SignatureBytes,
}

impl Default for SignedTransaction { 
    fn default() -> Self {
        SignedTransaction { 
            transaction: Transaction::default(), 
            signature: SignatureBytes([0; Signature::BYTE_SIZE])
        }
    }
}


impl ChainDigest for Transaction {
    fn digest(&self) -> Sha256Bytes {
        let bytes = bytes_of(self);
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

impl Transaction {
    // consumes
    pub fn sign(self, keypair: &Keypair) -> SignedTransaction {
        let digest = self.digest();
        let sig = keypair.sign(digest.as_slice());
        let sig_bytes = sig.to_bytes();
        SignedTransaction {
            transaction: self,
            signature: SignatureBytes(sig_bytes),
        }
    }
}

impl SignedTransaction {
    pub fn verify(&self) -> Result<(), SignatureError> {
        let digest = self.transaction.digest();
        let publickey = PublicKey::from_bytes(self.transaction.address.as_slice())?;
        let sig = Signature::from_bytes(self.signature.0.as_slice())?;
        publickey.verify(digest.as_slice(), &sig)
    }
}

#[repr(C)]
#[derive(Pod, Zeroable, Copy, Debug, Clone, Serialize, Deserialize)]
pub struct Transactions(pub [SignedTransaction; TXS_PER_BLOCK]);

impl ChainDigest for Transactions {
    fn digest(&self) -> Sha256Bytes {
        let bytes = bytemuck::bytes_of(self);
        let mut hasher = Sha256::new();
        hasher.update(bytes); // copy
        hasher.finalize().as_slice().try_into().unwrap()
    }
}

impl Default for Transactions { 
    fn default() -> Self {
        Transactions([SignedTransaction::default(); TXS_PER_BLOCK])
    }
}

/* BLOCK STRUCTS */
// we use serde for rpc 

#[repr(C)]
#[derive(Pod, Zeroable, Copy, Debug, Default, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    pub parent_hash: Sha256Bytes,
    pub state_root: Sha256Bytes,
    pub tx_root: Sha256Bytes,
    pub block_hash: Sha256Bytes,
    pub nonce: u128,
    pub height: u128,
}

impl ChainDigest for BlockHeader { 
    fn digest(&self) -> Sha256Bytes {
        self.block_hash
    }
}

#[repr(C)]
#[derive(Pod, Zeroable, Copy, Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub txs: Transactions,
}


// impl Serialize for Block {  
//     fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
//         where
//             S: serde::Serializer {
        
//     }
// }


impl Block {
    pub fn genesis() -> Self {
        let header = BlockHeader::genesis();
        let txs = Transactions::default();
        Block { header, txs }
    }
}

impl ChainDigest for Block { 
    fn digest(&self) -> Sha256Bytes {
        self.header.digest()
    }
}

impl BlockHeader {
    pub fn genesis() -> Self {
        let mut block = BlockHeader {
            state_root: [0; HASH_BYTE_SIZE],
            tx_root: [0; HASH_BYTE_SIZE],
            parent_hash: [0; HASH_BYTE_SIZE],
            block_hash: [0; HASH_BYTE_SIZE],
            nonce: 0,
            height: 0,
        };
        block.commit_block_hash();
        block
    }

    pub fn commit_block_hash(&mut self) {
        self.block_hash = self.compute_block_hash();
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

// #[cfg(test)] 
// mod tests { 
//     #[test]
//     pub fn test_zero_copy() { 

//     }
// }