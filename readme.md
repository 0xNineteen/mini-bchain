## mini-bchain 

a mini blockchain written in rust for learning purposes :)

## resources 
- how we manage chain state: [ETH State Managment](https://github.com/0xNineteen/blog.md/blob/master/contents/eth-state/index.md)

## files 
- `src/structures.rs`: defns of Accounts, Transactions, and Blocks
- `src/fork_choice.rs`: keeps track of the chain + its head
- `src/network.rs`: libp2p gossip 
- `src/pow.rs`: produces blocks by solving pow 
- `src/machine.rs`: state transition machine fcns (process txs to get new state)
- `src/db.rs`: manages chain state
- `src/rpc.rs`: manages the rpc api for the node
- `src/peer_manager.rs`: manages the peers connected through Mdns and stores their RPC client

- `bin/node.rs`: main node loop
- `bin/client.rs`: main client loop (sending txs)

## architecture 

![](imgs/2023-03-13-16-14-12.png)

### `BlockProducer` 
- `src/pow.rs`
  - grabs transactions from its mempool
  - gets head of chain from `ForkChoice`
  - processes them to get a new state (`src/machine.rs`)
  - do POW loop until block is solved 
    - if solved send block to `ForkChoice` to update chain/head and `P2P` to gossip to other nodes
    - periodically check `ForkChoice` for a new head

### `ForkChoice` 
- `src/fork_choice.rs`
  - keeps track of the head of the chain 
  - gets new blocks from pow and p2p network 

### `P2P Network` 
- `src/network.rs`
  - sets up gossip sub using libp2p 
  - listens for new txs and blocks from the network 
    - on new tx: send to `BlockProducer` to save in mempool
    - on new block: send to `ForkChoice` to update chain/head

## validator initialization 
- `ValidatorsAccount`: keeps track of all the validators in the network


## logging/metrics
- influxdb + grafana 
- solana-metrics 
- `brew services start influxdb`
  - setup on `http://localhost:8086`
  - create a bucket (`tmp`) through UI 
  - setup cli/http auth [https://docs.influxdata.com/influxdb/v2.4/tools/influx-cli/#set-up-the-influx-cli](https://docs.influxdata.com/influxdb/v2.4/tools/influx-cli/#set-up-the-influx-cli) with tmp bucket ID
    - `influx v1 auth create --username username --password password --token $TOKEN --read-bucket $ID --write-bucket $ID`
  - update `config.idb` 
- `brew services start grafana`
  - seutp a new datasource with influxdb + username password
- to collect metrics use `SOLANA_METRICS_CONFIG=$(cat config.idb) cargo r`

on shutdown 
- `brew services stop influxdb`
- `brew services stop grafana`

### notes 
- need to use depreceiated libp2p PeerID bc of solana-metrics dependency requirements

